# Built-in Cloudflare Python Workers

```sh
celld dev examples/python
```

Use this fork's binary and the separate `python-runtime` artifact directory.
No Python, Node, esbuild, pip, or pycelld executable is needed to develop, deploy,
or serve the worker. `celld deploy` uses the normal deployment flags and S3.

The default Python backend uses Cloudflare's `workers` SDK and CPython/Pyodide
314.0.6. The compiler and runtime hooks are built into celld. Interpreter bytes,
stdlib, the baseline snapshot, and declared WASM wheels are separate artifacts.
Source edits rebuild in Rust; invalid syntax leaves the last deployment serving.
Requests, responses and bindings cross the Pyodide FFI directly.

## Install or load runtime artifacts

Unpack the independently distributed runtime archive beside the binary:

- `celld`
- `python-runtime/runtime.json` and the files listed in that manifest

Alternatively, set `CELLD_PYTHON_RUNTIME` to an installed artifact directory.
A project can pin a runtime manifest from an HTTPS mirror (including an S3
bucket with HTTPS read access) in `pyproject.toml`:

```toml
[tool.celld.python-runtime]
manifest = "https://your-artifact-host/python/your-version/runtime.json"
sha256 = "FULL_64_CHARACTER_MANIFEST_SHA256"
```

The manifest pins every file's length, SHA-256 and interpretation. celld downloads
missing files in-process, validates them, and caches the complete artifact under
`$XDG_CACHE_HOME/celld/python-runtimes/<manifest-sha256>` (or `~/.cache/celld`).
Subsequent builds work offline. A project may also pin a local manifest path.
Corrupt build caches fail explicitly. An unsupported runtime ABI is refused.
The default artifact directory can also be installed under
`$XDG_CACHE_HOME/celld/python-runtime`.

There is no floating public CDN runtime chosen at request time. Draft CI artifacts
are available before a runtime release is published; the HTTPS mirror example
requires an actual published artifact and its checksum.

To produce the separate artifact from source:

```sh
python3 tools/python-runtime/build.py target/lab/python-runtime
cargo build -p celld --profile lab --locked
```

Only the artifact builder needs Python 3.11+ and Node/npm. Building the native
binary itself no longer downloads or embeds Pyodide.

## Reuse across deployments

The deployment entrypoint holds app sources and package declarations. Runtime
modules and each wheel are stored once per content hash at
`modules/sha256/<full-sha256>` in the fleet's existing bucket. A source-only edit
publishes a new small entrypoint and manifest while reusing the same runtime and
wheel objects. New nodes fetch referenced modules from S3 during deployment load;
Python initialization happens on the first invocation in each isolate.

Nodes verify bytes before execution and maintain a disposable local module cache
with a soft 1 GiB limit, evicting oldest writes first. Set `CELLD_MODULE_CACHE` to
choose its directory. Corrupt local entries are fetched again from S3; corrupt
bucket objects fail loading. Native compiled-WASM caching also remains in use.
This cache shares immutable code, not Python heaps or application state.

Serving needs only celld and S3. Serving nodes need neither the build-side runtime
installation nor access to a public package registry. Preloading a deployment
fetches its files, but does not initialize every Python isolate. Snapshots still
trade extra storage/memory for shorter interpreter startup. There is no automatic
bucket garbage collector yet: retain shared objects referenced by active or
rollback deployments. Nodes predating `shared-modules-v1` refuse these deployments.

## Declared libraries and supported scope

Declare packages under `[project].dependencies` in `pyproject.toml`, e.g.
`dependencies = ["pydantic>=2.12,<3", "numpy>=2"]`. The compiler resolves against
the pinned Pyodide catalog, checks constraints and WASM target markers, and
fetches matching wheels plus catalog dependencies. Downloads are checked and
cached under `.celld/python-cache`; each wheel becomes a shared module. Package
extras, URL requirements, custom wheels and dynamic dependencies currently need
a compiler extension and are refused by the default compiler.

This backend supports `WorkerEntrypoint.fetch`, request/response helpers,
outbound `workers.fetch`, vars and compatible native binding objects. Python
Durable Object classes, queue consumers and Workflow entrypoints are not wired
into the dispatcher. Unsupported entrypoint configurations fail during building.
Wrangler configuration support does not implement the Wrangler CLI's Cloudflare
upload/control-plane protocol or every Cloudflare service.

The separate `celld` Python SDK provides the function/decorator API, Pydantic DI,
generated clients and state primitives. Its CLI owns that build workflow; celld
never invokes it. Extensions can register in-process Rust `BuildHooks` through
`deploy::build_with_hooks` and `dev::run_with_hooks`, including shared ES, text
and WASM module output. Hooks prepare projects, replace compilers or transform
output before hashing/publication. The Python/JS compilers remain the defaults
when hooks return `None`.
