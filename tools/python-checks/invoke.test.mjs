import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createInvoker} from '../../crates/celld/python/invoke.mjs';

const deferred = () => {
  let resolve;
  const promise = new Promise(r => {resolve = r;});
  return {promise, resolve};
};

test('one bootstrap for concurrent calls; warm synchronous results stay synchronous', async () => {
  const gate = deferred();
  let boots = 0, lookups = 0;
  const runtime = {prefix:'hello', get fetch() {
    lookups++;
    return function(request, env, ctx) {return [this.prefix, request, env, ctx];};
  }};
  const fetch = createInvoker({initializeRuntime:async () => {boots++; await gate.promise; return {runtime};}});
  const a = fetch('a', 'ea', 'ca'), b = fetch('b', 'eb', 'cb');
  gate.resolve();
  assert.deepEqual(await a, ['hello','a','ea','ca']);
  assert.deepEqual(await b, ['hello','b','eb','cb']);
  assert.deepEqual(fetch('c', 'ec', 'cc'), ['hello','c','ec','cc']);
  assert.equal(boots, 1);
  assert.equal(lookups, 1);
});

test('sync and async bootstrap failures reject callers and permit retry', async () => {
  for (const asynchronous of [false,true]) {
    let boots = 0;
    const failure = new Error('boot failed');
    const fetch = createInvoker({initializeRuntime:() => {
      if (++boots === 1) {
        if (asynchronous) return Promise.reject(failure);
        throw failure;
      }
      return {runtime:{fetch:() => 'ok'}};
    }});
    await assert.rejects(fetch(), error => error === failure);
    assert.equal(await fetch(), 'ok');
    assert.equal(boots, 2);
  }
});

test('hooks bracket the full asynchronous invocation with separate concurrent contexts', async () => {
  const events = [], gate = deferred();
  const fetch = createInvoker({
    initializeRuntime:() => ({runtime:{async fetch(request, env, ctx) {
      events.push(['invoke',request,env,ctx]);
      if (request === 'a') await gate.promise;
      return request;
    }}}),
    beforeInvoke:async call => {events.push(['before',call.request,call.env,call.ctx]);},
    afterInvoke:call => {events.push(['after',call.request,call.env,call.ctx]);},
  });
  const a = fetch('a','ea','ca');
  assert.equal(await fetch('b','eb','cb'), 'b');
  assert.equal(events.some(e => e[0] === 'after' && e[1] === 'a'), false);
  gate.resolve();
  assert.equal(await a, 'a');
  for (const id of ['a','b']) {
    assert.deepEqual(events.filter(e => e[1] === id),
      ['before','invoke','after'].map(kind => [kind,id,'e'+id,'c'+id]));
  }
});

test('after hook runs on handler failure, not on before-hook failure', async () => {
  let after = 0;
  const failure = new Error('handler');
  const fetch = createInvoker({initializeRuntime:() => ({runtime:{fetch() {throw failure;}}}),
    afterInvoke:() => {after++;}});
  await assert.rejects(fetch(), error => error === failure);
  assert.equal(after, 1);
  const blocked = createInvoker({initializeRuntime:() => ({runtime:{fetch() {assert.fail('invoked');}}}),
    beforeInvoke:() => {throw failure;}, afterInvoke:() => {assert.fail('after');}});
  await assert.rejects(blocked(), error => error === failure);
});

test('after hook rejection takes precedence as with finally', async () => {
  const failure = new Error('after');
  const fetch = createInvoker({initializeRuntime:() => ({runtime:{fetch:() => 'ok'}}),
    afterInvoke:async () => {throw failure;}});
  await assert.rejects(fetch(), error => error === failure);
});
