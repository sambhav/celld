import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AsyncLocalStorage} from 'node:async_hooks';
import {schedulePythonCallback, withPythonSchedulingContext} from '../../crates/celld/python/scheduling-context.mjs';

test('ending one request cannot cancel another request scheduler wakeup', async () => {
  const requests = new AsyncLocalStorage();
  const original = globalThis.setTimeout;
  const timers = [];
  globalThis.setTimeout = (callback, delay) => {
    const owner = requests.getStore();
    timers.push({owner, run:() => requests.run(owner, callback), delay});
  };
  try {
    let a = 0, b = 0;
    requests.run('a', () => withPythonSchedulingContext(() => {
      for (let i=0; i<65; i++) schedulePythonCallback(() => a++);
    }));
    const finished = requests.run('b', () => withPythonSchedulingContext(() => new Promise(resolve => {
      const next = () => ++b === 130 ? resolve() : schedulePythonCallback(next);
      schedulePythonCallback(next);
    })));
    // Drain V8 microtasks, then simulate the host dropping request A's timers.
    // B still has its own timer even though A exhausted its budget first.
    for (let round=0; round<400 && b<130; round++) {
      await Promise.resolve();
      const next = timers.shift();
      if (next?.owner === 'b') next.run();
    }
    assert.equal(a, 64);
    assert.equal(b, 130);
    await finished;
  } finally {
    globalThis.setTimeout = original;
  }
});
