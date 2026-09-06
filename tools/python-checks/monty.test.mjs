import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

// Node has no cloudflare: module. Supply only its base-class contract; the
// production adapter itself is executed unchanged apart from that import URL.
const cf = 'data:text/javascript,'+encodeURIComponent('export class DurableObject {constructor(ctx,env){this.ctx=ctx;this.env=env}}');
const source = (await readFile(new URL('../../crates/celld/python/monty.mjs',import.meta.url),'utf8'))
  .replace("'cloudflare:workers'", JSON.stringify(cf));
const {createMontyWorker,createMontyObject}=await import('data:text/javascript,'+encodeURIComponent(source));
const {DurableObject}=await import(cf);
const manifest=[{name:'increment',parameters:{type:'object',properties:{},required:[]}}, {name:'alarm',parameters:{}}];
const invocation = body => new Request('http://local/increment',{method:'POST',body:JSON.stringify(body)});

test('durable classes use the native RPC base and consume alarm event metadata',async()=>{
  const requests=[];
  globalThis.__monty = input => {
    const r=JSON.parse(input); requests.push(r);
    return JSON.stringify(r.action==='compile'?{module:10}:r.action==='drop'?null:{id:20,done:true,result:42});
  };
  let gates=0;
  const ctx={id:{toString:()=> 'object-id',name:'counter'},blockConcurrencyWhile:async f=>{gates++;return f()}};
  const Class=createMontyObject('',manifest,'Counter');
  assert.ok(new Class(ctx,{}) instanceof DurableObject);
  const instance=new Class(ctx,{});
  assert.equal(await instance.increment({amount:2}),42);
  await instance.alarm({scheduledTime:100,retryCount:2,isRetry:true});
  const alarm=requests.filter(r=>r.action==='start').at(-1);
  assert.deepEqual(alarm.args,{});
  assert.equal(alarm.context.alarm.retryCount,2);
  assert.equal(gates,2);
  assert.equal(requests.filter(r=>r.action==='compile').length,1,'compiled module is reused');
  assert.equal(requests.filter(r=>r.action==='drop').length,2);
});

test('an interpreter failure rolls back an open native storage transaction',async()=>{
  let step=0,drops=0;
  globalThis.__monty = input => {
    const r=JSON.parse(input);
    if(r.action==='compile')return '{"module":1}';
    if(r.action==='drop'){drops++;return 'null'}
    if(step++===0)return JSON.stringify({id:2,done:false,operation:'storage.transaction_begin',args:[]});
    if(step===2)return JSON.stringify({id:2,done:false,operation:'storage.put',args:['count',99]});
    return '{"error":"interpreter budget exceeded"}';
  };
  const state=new Map([['count',1]]);
  const storage={async transaction(callback){
    const original=new Map(state);
    await callback({async put(k,v){state.set(k,v)},rollback(){state.clear();for(const entry of original)state.set(...entry)}});
  }};
  const Class=createMontyObject('',manifest,'Counter');
  await assert.rejects(new Class({storage,blockConcurrencyWhile:f=>f()},{}).increment({}),/budget exceeded/);
  assert.equal(state.get('count'),1);
  assert.equal(drops,1,'failed continuation is released');
});

test('storage is unavailable to stateless functions and errors resume Python',async()=>{
  const requests=[];
  globalThis.__monty = input => {
    const r=JSON.parse(input);requests.push(r);
    if(r.action==='compile')return '{"module":1}';
    if(r.action==='drop')return 'null';
    if(r.action==='start')return JSON.stringify({id:2,done:false,operation:'storage.get',args:['x']});
    return JSON.stringify({id:2,done:true,result:r.reply.error});
  };
  const worker=createMontyWorker('',manifest);
  const response=await worker.fetch(invocation({}),{},{});
  assert.equal((await response.json()).result,'storage requires a durable object');
  const description=await worker.fetch(new Request('http://local/__celld/schema'),{},{});
  assert.deepEqual(Object.keys((await description.json()).functions),['increment']);
  assert.equal((await worker.fetch(new Request('http://local/alarm'),{},{})).status,404);
  assert.equal(requests.at(-1).action,'drop');
});

test('sync cannot wait for durability from inside its own transaction',async()=>{
  let step=0,observed;
  globalThis.__monty = input => {
    const r=JSON.parse(input);
    if(r.action==='compile')return '{"module":1}';
    if(r.action==='drop')return 'null';
    if(step++===0)return JSON.stringify({id:2,done:false,operation:'storage.transaction_begin',args:[]});
    if(step===2)return JSON.stringify({id:2,done:false,operation:'storage.sync',args:[]});
    observed=r.reply.error;
    return '{"error":"transaction stopped"}';
  };
  const storage={async transaction(f){await f({rollback(){},sync(){assert.fail('would deadlock')}})}};
  const Class=createMontyObject('',manifest,'Counter');
  await assert.rejects(new Class({storage,blockConcurrencyWhile:f=>f()},{}).increment({}),/transaction stopped/);
  assert.match(observed,/not allowed inside a storage transaction/);
});
