// Keep the warm path synchronous until Python actually returns an awaitable.
// The bound callable also owns its PyProxy for the lifetime of this isolate.
export function createInvoker({initializeRuntime, beforeInvoke, afterInvoke}) {
  let boot;
  let invoke;

  function ready() {
    return boot ??= Promise.resolve().then(initializeRuntime).then(runtime => {
      invoke = runtime.runtime.fetch.bind(runtime.runtime);
      return invoke;
    }).catch(error => {
      boot = undefined;
      throw error;
    });
  }

  if (!beforeInvoke && !afterInvoke) {
    return (request, env, ctx) => invoke
      ? invoke(request, env, ctx)
      : ready().then(fetch => fetch(request, env, ctx));
  }

  return async (request, env, ctx) => {
    const fetch = invoke ?? await ready();
    const call = {request, env, ctx};
    if (beforeInvoke) await beforeInvoke(call);
    try {
      return await fetch(request, env, ctx);
    } finally {
      if (afterInvoke) await afterInvoke(call);
    }
  };
}
