// celld 0.4.0 does not drive V8's asynchronous WASM compilation tasks.
// Keep Pyodide's promise API while compiling synchronously inside the isolate.
// The main Python module is imported separately and shared by celld's cache.
const native = globalThis.WebAssembly;
export const WebAssembly = Object.create(native);
WebAssembly.compile = async bytes => new native.Module(bytes);
WebAssembly.instantiate = async (input, imports) => {
  if (input instanceof native.Module) return new native.Instance(input, imports);
  const module = new native.Module(input);
  return { module, instance: new native.Instance(module, imports) };
};
WebAssembly.compileStreaming = async input => WebAssembly.compile(await (await input).arrayBuffer());
WebAssembly.instantiateStreaming = async (input, imports) =>
  WebAssembly.instantiate(await (await input).arrayBuffer(), imports);
