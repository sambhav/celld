# Python runtime artifact provenance

- Pyodide 314.0.6, official npm distribution, including CPython 3.14.2.
  Exact asset checksums: `tools/python-runtime/runtime-lock.json`.
- `workers/*.py`: unmodified Cloudflare workers-runtime-sdk 1.8.3 sources from
  https://github.com/cloudflare/workers-py/tree/1611584709e4d303088e27c2843ec81faf122baa/packages/runtime-sdk/src/workers
  Copyright Cloudflare, Inc. and contributors. MIT license, as declared by the
  upstream repository README and package metadata.
- Loader adapter, snapshot generator and asset fetch originate in
  https://github.com/sambhav/celld-python (Apache-2.0).
- Snapshot creation uses Pyodide's upstream API. It includes interpreter/stdlib
  state only. User sources and third-party packages load after restore.

The separate artifact builder uses Python/Node/npm to assemble the runtime.
The native binary compiles application projects in Rust and serves their
referenced runtime modules through V8/WASM, without those build tools.
