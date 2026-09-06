import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AsyncLocalStorage} from 'node:async_hooks';
import {createScheduler} from '../../crates/celld/python/schedule.mjs';

test('ready callbacks are deferred, FIFO, once each; positive delays remain timers', () => {
  const microtasks = [], timers = [], seen = [];
  const schedule = createScheduler({microtask:f => microtasks.push(f), timer:(f,ms) => timers.push([f,ms])});
  schedule(() => seen.push(1));
  schedule(() => seen.push(2), 0);
  schedule(() => seen.push(3), 1);
  assert.deepEqual(seen, []);
  assert.equal(timers[0][1], 1);
  microtasks.forEach(f => f());
  assert.deepEqual(seen, [1,2]);
  timers[0][0]();
  assert.deepEqual(seen, [1,2,3]);
});

test('a chain of immediately ready work yields after its finite budget', () => {
  const microtasks = [], timers = [];
  const schedule = createScheduler({budget:4, microtask:f => microtasks.push(f), timer:f => timers.push(f)});
  let count = 0;
  const again = () => {count++; schedule(again);};
  schedule(again);
  while (microtasks.length) microtasks.shift()();
  assert.equal(count, 4);
  assert.equal(timers.length, 1);
  timers.shift()();
  while (microtasks.length) microtasks.shift()();
  assert.equal(count, 9);
  assert.equal(timers.length, 1);
});

test('overlapping callbacks retain their request async context on both paths', async () => {
  const context = new AsyncLocalStorage();
  // celld's queueMicrotask fallback uses a promise reaction; exercise that too.
  for (const microtask of [queueMicrotask, f => Promise.resolve().then(f)]) {
    const schedule = createScheduler({budget:2, microtask});
    await Promise.all(Array.from({length:12}, (_, id) => context.run(id, () => new Promise((resolve,reject) => {
      schedule(() => {
        try {assert.equal(context.getStore(), id);} catch (error) {reject(error); return;}
        schedule(() => {
          try {assert.equal(context.getStore(), id); resolve();} catch (error) {reject(error);}
        });
      });
    }))));
  }
});

test('real timer progresses while a ready chain continues', async () => {
  const schedule = createScheduler({budget:4});
  let count = 0, timerCount;
  const timer = new Promise(resolve => setTimeout(() => {timerCount = count; resolve();}, 0));
  await new Promise(resolve => {
    const next = () => ++count === 100 ? resolve() : schedule(next);
    schedule(next);
  });
  await timer;
  assert.ok(timerCount < 100, `timer starved until callback ${timerCount}`);
});
