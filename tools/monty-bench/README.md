# Monty feasibility experiment

> These are historical standalone experiments. The integrated Pyodide/Monty
> backends and current API are documented in [the runtime guide](../../docs/python.md).
> Standalone benchmark results do not establish integrated celld performance.


The [public-function prototype](FUNCTIONS.md) now exposes ordinary module
functions, generates a Python client and tests async gather, strict inputs,
stateful snapshot continuation and actual Pydantic wheel ABI requirements.

[Measured results and engineering decision](results/2026-09-06.md): the native
prototype reaches approximately celld TypeScript throughput at four threads,
with much lower RSS, but is not yet a celld backend. Warm function costs do
not establish a faster interpreter, and Pydantic/WASM wheel imports are absent.

A bounded experiment to decide whether an optional Monty backend is worth
building alongside full Pyodide. It does not change celld's backend or SDK.
Monty is pinned to `af272c3116e2525249103f960b79086fd250bcef`; Cargo.lock pins
transitive dependencies. The `benchmark-monty` draft PR label runs it on GitHub.

`worker.py` includes a decorator, typed function, JSON validation and the same
pricing transformation as `../python-bench/worker.py`. The Rust host parses
and compiles once per execution thread, clones the compiled program into a
fresh heap per request, then suspends at `get_price` while Rust awaits real
HTTP. It returns the result to Python and resumes execution. No CPython host,
JS bridge, Monty subprocess, or interpreter modification is involved.

The separate Rust control uses the same axum server and reqwest client with
Rust business logic. Python/TypeScript controls run on the same runner through
the actual celld binary. All responses use unique inputs, are validated by
the existing Go driver, and must have zero errors and exact upstream counts.
Three five-second rounds rotate backend order at one/four execution slots and
256 clients. Upstream delays are 10/50 ms. Artifacts include raw samples,
CPU model, executable hashes, source revision, latency and RSS.

Important limits:

- This is a fixed-code, in-process prototype, not a production host for
  untrusted code. Monty recommends its subprocess pool for crash isolation.
  No allocator-backed memory bound is claimed here; a two-second interpreter
  deadline and at most one allowed host call bound this fixture. The upstream
  URL is supplied by the harness, never by request code.
- Tokio threads and celld isolate limits are different execution mechanisms.
  The standalone server bypasses celld routing and worker lifecycle. Its
  throughput cannot establish performance of a future celld integration.
- All processes share the GitHub CPU, including load generator and upstream.
  The Rust control helps locate the ceiling of this apparatus, not a server
  running alone. Celld uses SQLite dev storage and warm artifact disk caches.
- Fresh process readiness and the first request are measured separately.
  This is not a remote S3 deployment or a measured idle scale-to-zero wake.
- Microprobes compare parsing/compilation, fresh-heap execution, suspendable
  start, a warm REPL function call and snapshot restore. CPython/Pyodide/V8
  microloops are additional context, not equivalent RPC benchmarks. The
  Pyodide microloop uses stock Pyodide in Node, not celld's JSPI/snapshot host.
- Compatibility probes report actual outcomes for decorators, dataclasses,
  Pydantic, NumPy, inspection, function metadata and asyncio.sleep. Async
  pending-future resume and mutable-state snapshot restore are asserted.
  No SDK, Pydantic validation or WASM wheel compatibility is implied.

Run:

```sh
cargo run --release --locked --manifest-path tools/monty-bench/Cargo.toml -- micro
python tools/monty-bench/measure.py --celld native-bin/celld \
  --runtime candidate-runtime \
  --monty tools/monty-bench/target/release/celld-monty-experiment
```
