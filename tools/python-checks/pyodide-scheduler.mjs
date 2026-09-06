// Exercise the real pinned CPython/WASM once-callables, not JS stand-ins.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {resolve} from 'node:path';
import {createScheduler} from '../../crates/celld/python/schedule.mjs';
import {createInvoker} from '../../crates/celld/python/invoke.mjs';

const runtime = resolve(process.argv[2]);
const {loadPyodide} = await import(pathToFileURL(`${runtime}/pyodide.mjs`));
const py = await loadPyodide();
const webloop = py.pyimport('pyodide.webloop');
webloop.scheduleCallback = createScheduler({microtask:f => Promise.resolve().then(f), budget:4});
webloop.destroy();
let deadline;
try {
  const result = await Promise.race([
    new Promise((_,reject) => {deadline = setTimeout(() => reject(new Error('Python scheduler stalled')), 5000);}),
    py.runPythonAsync(`
import asyncio, contextvars
from pyodide.ffi import create_once_callable
from pyodide.webloop import scheduleCallback

seen = []
scheduleCallback(create_once_callable(lambda: seen.append('once')), 0)
current = contextvars.ContextVar('current')
async def child(index):
    token = current.set(index)
    try:
        for _ in range(20):
            await asyncio.sleep(0)
            assert current.get() == index
        await asyncio.sleep(.001)
        return index
    finally:
        current.reset(token)
assert await asyncio.gather(*(child(i) for i in range(12))) == list(range(12))
assert seen == ['once'], seen
task = asyncio.create_task(asyncio.sleep(10))
await asyncio.sleep(0)
task.cancel()
try:
    await task
except asyncio.CancelledError:
    pass
else:
    raise AssertionError('cancelled task completed')
'PASS'
`),
  ]);
  assert.equal(result, 'PASS');
  const runtimeObject = py.runPython(`
class Invocation:
    async def fetch(self, request, env, ctx):
        await asyncio.sleep(0)
        return request + env.suffix + ctx.suffix
Invocation()
`);
  const fetch = createInvoker({initializeRuntime:() => ({runtime:runtimeObject})});
  assert.deepEqual(await Promise.all([
    fetch('one', {suffix:'-env1'}, {suffix:'-ctx1'}),
    fetch('two', {suffix:'-env2'}, {suffix:'-ctx2'}),
  ]), ['one-env1-ctx1','two-env2-ctx2']);
  assert.equal(await fetch('warm', {suffix:'-env'}, {suffix:'-ctx'}), 'warm-env-ctx');
  console.log('Real Pyodide scheduler, strict once-callable, contextvars, gather, timers, cancellation: PASS');
} finally {
  clearTimeout(deadline);
}
// Pyodide owns a persistent event loop; this standalone test has completed.
process.exit(0);
