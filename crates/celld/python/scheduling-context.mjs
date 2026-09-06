import {AsyncLocalStorage} from 'node:async_hooks';
import {createScheduler} from './schedule.mjs';

const context = new AsyncLocalStorage();
const bootstrap = createScheduler();

// A celld native timer belongs to the current request and is cancelled when
// that request ends. A shared yield gate would therefore strand unrelated
// requests. V8 propagates this frame alongside the native I/O context.
export function withPythonSchedulingContext(callback) {
  return context.run(createScheduler(), callback);
}

export function schedulePythonCallback(callback, delay=0) {
  return (context.getStore() ?? bootstrap)(callback, delay);
}
