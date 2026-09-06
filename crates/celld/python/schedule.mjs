// Pyodide's browser scheduler falls back to a native timer for every asyncio
// continuation in celld. There is no browser UI here: ready work can use V8's
// microtask queue, like JS promises. Preserve each callback's async context;
// never batch different requests inside one callback. A finite budget yields
// to the host even when Python repeatedly awaits asyncio.sleep(0).
export function createScheduler({microtask=queueMicrotask, timer=setTimeout, budget=64}={}) {
  if (!Number.isSafeInteger(budget) || budget < 1) throw new RangeError('Invalid scheduler budget');
  let remaining = budget;
  return (callback, delay=0) => {
    if (delay > 0) return timer(callback, delay);
    if (remaining > 0) {
      remaining--;
      return microtask(callback);
    }
    return timer(() => {
      remaining = budget;
      callback();
    }, 0);
  };
}

export const schedulePythonCallback = createScheduler();
