# Built-in Cloudflare Python Workers

```sh
celld dev examples/python
```

Use the fork's binary. No Python, Node, esbuild, pip, or pycelld installation is
needed to develop, deploy, or serve this worker. `celld deploy examples/python`
uses the normal celld deployment flags and S3 storage.

The default Python backend uses Cloudflare's `workers` SDK and CPython/Pyodide
314.0.6, embedded in the binary with a compressed interpreter snapshot. Source
edits rebuild in Rust; invalid Python syntax leaves the last deployment serving.
Requests, responses and bindings cross the Pyodide FFI directly.

Declare packages under `[project].dependencies` in `pyproject.toml`, e.g.
`dependencies = ["pydantic>=2.12,<3", "numpy>=2"]`. The compiler resolves against
the pinned Pyodide catalog, checks version constraints and target markers, and
bundles the matching WASM wheels and their catalog dependencies. Downloaded
packages are cached by checksum under `.celld/python-cache`. Deployment nodes
perform no package downloads. Package extras, URL requirements and custom wheels
currently need a compiler extension.

This first built-in backend supports the default `WorkerEntrypoint.fetch`
handler, request/response helpers, outbound `workers.fetch`, vars and compatible
native binding objects. Python Durable Object classes, queue consumers and
Workflow entrypoints are not yet wired into the dispatcher. Unsupported entrypoint
configurations fail during bundling. A matching API surface does not imply every
Cloudflare service is provided by celld.

The separate `celld` Python SDK provides the function/decorator API, Pydantic DI,
generated clients and state primitives. Its CLI owns that build workflow; celld
never invokes it. Extensions can register in-process Rust `BuildHooks` through
`deploy::build_with_hooks` and `dev::run_with_hooks`. Hooks can prepare a project,
replace a compiler, or transform output before deployment hashing/publication.
The built-in Python/JS compilers are the defaults when hooks return `None`.

To build celld itself from source, install Rust, Python 3.11+, and Node/npm.
The Cargo build script prepares and verifies the embedded runtime assets once.
These tools are not needed with the resulting binary.
