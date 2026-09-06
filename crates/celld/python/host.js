import {loadPyodide} from './pyodide.mjs';
import createPyodideModule from './pyodide.asm.mjs';
import {assetFetch, registerAssets} from './assets.js';
import {workers, dispatch} from './python-sources.js';
import * as cloudflareWorkersModule from 'cloudflare:workers';
import * as cloudflareSocketsModule from 'cloudflare:sockets';
import {createInvoker} from './invoke.mjs';
import {withPythonSchedulingContext} from './scheduling-context.mjs';

async function initialize(manifest, assets) {
  registerAssets(assets);
  const response = await assetFetch('https://celld-python.invalid/runtime/baseline.snapshot.gz');
  const snapshot = new Uint8Array(await new Response(response.body.pipeThrough(new DecompressionStream('gzip'))).arrayBuffer());
  const py = await loadPyodide({_loadSnapshot:snapshot, indexURL:'https://celld-python.invalid/runtime/',
    lockFileContents:manifest.lock, packageBaseUrl:'https://celld-python.invalid/packages/',
    packages:Object.keys(manifest.lock.packages), createPyodideModule, enableRunUntilComplete:false});
  py.registerJsModule('_pyodide_entrypoint_helper', {
    cloudflareWorkersModule, cloudflareSocketsModule,
    // One FFI call for the common JSON response, with fresh native headers.
    jsonResponse: body => new Response(body, {headers:{'content-type':'application/json'}}),
    patchWaitUntil(ctx) {
      if (ctx.__pythonWaitUntil) return;
      const original = ctx.waitUntil.bind(ctx);
      ctx.waitUntil = value => original(Promise.resolve(value));
      ctx.__pythonWaitUntil = true;
    },
    doAnImport: name => import(name),
    patch_env_helper() { throw new Error('workers.patch_env is not supported by celld'); },
  });
  for (const [name, source] of Object.entries({...workers, ...manifest.sources, '_celld_dispatch.py':dispatch})) {
    const path = '/app/' + name;
    py.FS.mkdirTree(path.slice(0, path.lastIndexOf('/')));
    py.FS.writeFile(path, source);
  }
  py.FS.writeFile('/app/_cloudflare_compat_flags.py', 'def __getattr__(name):\n    return False\n');
  py.runPython("import sys, random; random.seed(); sys.path.insert(0, '/app')");
  const module = py.pyimport('_celld_dispatch');
  const runtime = module.Runtime(manifest.entrypoint);
  module.destroy();
  return {py, runtime};
}

const runtimes = new Map();
function sharedInitialize(manifest, assets) {
  const key = JSON.stringify(manifest);
  if (!runtimes.has(key)) {
    runtimes.set(key, initialize(manifest, assets).catch(error => {
      runtimes.delete(key);
      throw error;
    }));
  }
  return runtimes.get(key);
}

// Generic runtime hooks are supplied in-process by an extension. The default
// is the built-in Cloudflare Python backend, with one warm runtime per isolate.
export function createPythonWorker(manifest, {assets={}, initializeRuntime=()=>sharedInitialize(manifest, assets), beforeInvoke, afterInvoke}={}) {
  const invoke = createInvoker({initializeRuntime, beforeInvoke, afterInvoke});
  return {fetch:(request, env, ctx) => withPythonSchedulingContext(() => invoke(request, env, ctx))};
}

export function createPythonObject(manifest, className, methods, {assets={}}={}) {
  class PythonObject {
    constructor(ctx, env) {
      // Load lazily within the first event's I/O and scheduling context.
      this.ctx = ctx;
      this.env = env;
      this.instance = null;
    }
  }
  for (const name of methods) {
    if (name.startsWith('_') || name === 'constructor' || name === 'then')
      throw new Error(`reserved Python method: ${name}`);
    Object.defineProperty(PythonObject.prototype, name, {value: function(...args) {
      return withPythonSchedulingContext(async () => {
        const initialized = await sharedInitialize(manifest, assets);
        this.instance ??= initialized.runtime.construct(className, this.ctx, this.env);
        return await this.instance.call(name, args);
      });
    }});
  }
  return PythonObject;
}
