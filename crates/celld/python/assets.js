import { assets } from './asset-data.js';

// Lexically scoped to the Pyodide loader. Never patches global fetch, and
// never falls back to a CDN/PyPI: missing declared artifacts fail closed.
export async function assetFetch(input) {
  const url = new URL(typeof input === 'string' ? input : input.url ?? String(input));
  if (url.origin !== 'https://celld-python.invalid') throw new Error(`Unbundled runtime URL: ${url}`);
  const data = assets[url.pathname];
  if (data === undefined) throw new Error(`Unbundled runtime asset: ${url.pathname}`);
  let bytes;
  if (typeof Uint8Array.fromBase64 === 'function') {
    bytes = Uint8Array.fromBase64(data);
  } else {
    const binary = atob(data);
    bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  }
  return new Response(bytes, {
    headers: { 'content-type': url.pathname.endsWith('.wasm') ? 'application/wasm' : 'application/octet-stream' },
  });
}
