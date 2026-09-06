import {DurableObject} from 'cloudflare:workers';
// Lifecycle adapter only. Python compilation, execution and capability registration
// live in Rust; storage/alarms/RPC use celld's existing native-backed objects.
const native = request => {
  const result = JSON.parse(__monty(JSON.stringify(request)));
  if (result?.error) throw new Error(result.error);
  return result;
};
const plainEnv = env => Object.fromEntries(Object.entries(env).filter(([,v]) =>
  v === null || ['string','number','boolean'].includes(typeof v)));
const allowedName = name => typeof name === 'string' && !name.startsWith('_') &&
  !['constructor','then','fetch','alarm'].includes(name);

async function execute(module, name, args, env, ctx, signal, caller={}) {
  const transactions = [];
  let id;
  const currentStorage = () => {
    if (!ctx?.storage) throw new Error('storage requires a durable object');
    return transactions.at(-1)?.storage ?? ctx.storage;
  };
  const capability = async (op, a) => {
    if (transactions.length && (!op.startsWith('storage.') || op === 'storage.sync'))
      throw new Error('external I/O is not allowed inside a storage transaction');
    switch (op) {
      case 'storage.get': return await currentStorage().get(a[0]) ?? null;
      case 'storage.put': await currentStorage().put(a[0],a[1]); return null;
      case 'storage.delete': return await currentStorage().delete(a[0]);
      case 'storage.list': return Object.fromEntries(await currentStorage().list({prefix:a[0],limit:a[1],reverse:a[2]}));
      case 'storage.delete_all': await currentStorage().deleteAll(); return null;
      case 'storage.sql': return currentStorage().sql.exec(a[0],...a[1]).toArray();
      case 'storage.sync': await currentStorage().sync(); return null;
      case 'storage.get_alarm': return await currentStorage().getAlarm();
      case 'storage.set_alarm': await currentStorage().setAlarm(a[0]); return null;
      case 'storage.delete_alarm': await currentStorage().deleteAlarm(); return null;
      case 'storage.transaction_begin': {
        if (transactions.length >= 16) throw new Error('transaction nesting limit exceeded');
        const storage = currentStorage();
        let ready, finish;
        const started = new Promise(resolve => { ready = resolve; });
        const ended = new Promise(resolve => { finish = resolve; });
        const record = {finish};
        record.done = storage.transaction(async tx => {
          record.storage = tx;
          ready();
          if (!await ended) tx.rollback();
        });
        // Observe start failure as well as callback entry.
        await Promise.race([started, record.done.then(() => { throw new Error('transaction did not start'); })]);
        transactions.push(record);
        return null;
      }
      case 'storage.transaction_commit':
      case 'storage.transaction_rollback': {
        const transaction = transactions.pop();
        if (!transaction) throw new Error('no open transaction');
        transaction.finish(op.endsWith('_commit'));
        await transaction.done;
        return null;
      }
      case 'object.call': {
        const [binding,objectName,method,params] = a;
        if (!allowedName(method) || typeof objectName !== 'string') throw new Error('invalid object call');
        const namespace = Object.hasOwn(env,binding) && env[binding];
        if (!namespace?.getByName) throw new Error('unknown durable object binding');
        return await namespace.getByName(objectName)[method](params);
      }
      case 'fetch': {
        const response = await fetch(a[0], {method:a[1],headers:a[2],body:a[3] ?? undefined,signal});
        const body = await response.text();
        if (body.length > 1024 * 1024) throw new Error('fetch response exceeds 1 MiB');
        return {status:response.status,headers:Object.fromEntries(response.headers),body};
      }
      case 'sleep': {
        if (typeof a[0] !== 'number' || a[0] < 0 || a[0] > 30) throw new Error('sleep must be between 0 and 30 seconds');
        await new Promise((resolve,reject) => {
          const abort = () => { clearTimeout(timer); reject(new Error('request cancelled')); };
          const timer = setTimeout(() => { signal?.removeEventListener('abort',abort); resolve(); },a[0]*1000);
          signal?.addEventListener('abort',abort,{once:true});
          if (signal?.aborted) abort();
        });
        return null;
      }
      case 'now': return Date.now();
      case 'uuid': return crypto.randomUUID();
      case 'log': console.log(a[0]); return null;
      default: throw new Error('unregistered capability');
    }
  };
  try {
    let event = native({action:'start',module,name,args,context:{...caller,
      id:ctx?.id?.toString() ?? null,name:ctx?.id?.name ?? null,env:plainEnv(env)}});
    id = event.id;
    while (!event.done) {
      if (signal?.aborted) throw new Error('request cancelled');
      let reply;
      try { reply = {result:await capability(event.operation,event.args)}; }
      catch (error) { reply = {error:String(error.message ?? error)}; }
      event = native({action:'resume',id,reply});
    }
    if (transactions.length) throw new Error('unclosed transaction');
    return event.result;
  } finally {
    try {
      let failure;
      while (transactions.length) {
        const transaction = transactions.pop();
        transaction.finish(false);
        try { await transaction.done; } catch (error) { failure ??= error; }
      }
      if (failure) throw failure;
    } finally {
      if (id !== undefined) native({action:'drop',id});
    }
  }
}

export function createMontyWorker(source, manifest, className=null) {
  let module;
  const methods = new Set(manifest.map(f=>f.name).filter(allowedName));
  const schema = {version:1, runtime:'monty', functions:Object.fromEntries(manifest.filter(f=>methods.has(f.name)).map(f=>[f.name,{arguments:f.parameters,returns:{},context:{},stateful:false,replay:false}]))};
  return {async fetch(request,env,ctx) {
    const path = new URL(request.url).pathname;
    if (path === '/__celld/schema' && request.method === 'GET') return Response.json(schema);
    const match = /^\/(?:call\/)?([^/]+)$/.exec(path);
    const name = match && decodeURIComponent(match[1]);
    if (!methods.has(name)) return new Response('unknown function',{status:404});
    if (request.method !== 'POST') return new Response('POST required',{status:405});
    try {
      const body = await request.text();
      if (body.length > 1024 * 1024) return new Response('request too large',{status:413});
      const caller = {caller:JSON.parse(request.headers.get('x-celld-context') || '{}'), client:JSON.parse(request.headers.get('x-celld-client') || '{}'), call_id:request.headers.get('x-celld-call-id'), attempt:Number(request.headers.get('x-celld-attempt') || 1)};
      module ??= native({action:'compile',source,class:className}).module;
      return Response.json({result:await execute(module,name,JSON.parse(body),env,ctx,request.signal,caller)});
    } catch (error) { return Response.json({error:{code:'execution_error',message:String(error.message ?? error)}},{status:422}); }
  }};
}

export function createMontyObject(source, manifest, className) {
  let module;
  class MontyObject extends DurableObject {
    constructor(ctx,env) { super(ctx,env); }
  }
  for (const {name} of manifest) {
    if (!allowedName(name) && name !== 'alarm') throw new Error(`reserved method: ${name}`);
    Object.defineProperty(MontyObject.prototype,name,{value:function(args={}) {
      // A whole function call is one serial object turn. The native input gate
      // also covers awaited I/O, so read/modify/write cannot lose an update.
      return this.ctx.blockConcurrencyWhile(()=>{
        module ??= native({action:'compile',source,class:className}).module;
        return execute(module,name,args,this.env,this.ctx);
      });
    }});
  }
  return MontyObject;
}
