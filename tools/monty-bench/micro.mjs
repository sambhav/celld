import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {performance} from 'node:perf_hooks';
const root = path.resolve(process.argv[2]);
const {loadPyodide} = await import(pathToFileURL(`${root}/pyodide.mjs`));
const start = performance.now();
const py = await loadPyodide({indexURL: root + '/'});
const bootMs = performance.now()-start;
const source = fs.readFileSync('tools/monty-bench/worker.py','utf8');
py.runPython(source);
py.runPython(`import time, json
raw = '{"name":"benchmark"}'
assert json.loads(handle(raw, False)) == {'result':'Hello, benchmark'}
for _ in range(1000): handle(raw, False)
samples = []
for _ in range(5):
    start = time.perf_counter()
    for _ in range(10000): handle(raw, False)
    samples.append((time.perf_counter()-start)*1e9/10000)
`);
const wasm = JSON.parse(py.runPython('json.dumps(samples)'));
const handle = py.globals.get('handle');
const raw = '{"name":"benchmark"}';
const ffi = [];
for (let round=0;round<5;round++) {
  const start=performance.now();
  for (let i=0;i<10000;i++) handle(raw,false);
  ffi.push((performance.now()-start)*1e6/10000);
}
handle.destroy();
function hello(raw) {
  const name=JSON.parse(raw).name;
  if(typeof name!=='string'||name.length<1||name.length>128) throw new Error('invalid name');
  return JSON.stringify({result:'Hello, '+name});
}
if(JSON.parse(hello(raw)).result!=='Hello, benchmark') throw new Error('wrong result');
for(let i=0;i<1000;i++) hello(raw);
const js=[];
let result;
for(let round=0;round<5;round++) {
  const start=performance.now();
  for(let i=0;i<10000;i++) result=hello(raw);
  js.push((performance.now()-start)*1e6/10000);
}
if(JSON.parse(result).result!=='Hello, benchmark') throw new Error('wrong result');
console.log('WASM_MICRO='+JSON.stringify({boot_ms:bootMs,python_inner_loop_ns:wasm,js_to_py_call_ns:ffi,v8_js_ns:js,jspi:typeof WebAssembly.Suspending==='function'}));
process.exit(0);
