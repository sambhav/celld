# Native Python versus TypeScript

Matched Cloudflare `WorkerEntrypoint` and TypeScript fetch workers on the same
celld binary and GitHub runner. This measures the built-in Python backend;
the separate `celld` decorator/Pydantic SDK adds its own application overhead.
Python here is full CPython running in WASM, integrated into the native host.

Both implementations validate a unique request name and return JSON. The I/O
case also makes one real HTTP request to a concurrent Go pricing server,
validates its JSON response, computes a price, and includes the caller's trace.
The upstream delay is controlled; its responses are not cached or replayed.
Every response is checked and each measured I/O run must match the upstream's
request and completion counters exactly. Any mismatch fails the run.

The Go driver uses HTTP/1.1 keep-alive, fixed concurrent clients, no retries,
and unique call IDs. The driver and upstream share the runner's CPU with celld;
driver CPU consumption is recorded. These are whole-runner throughput numbers,
not a claim about isolated server CPU capacity. The object store is celld's
local SQLite development adapter, not remote S3. There are no durable calls.

Each sample starts a fresh celld process, checks the first response separately,
warms it with four requests per concurrent client, then starts the timed load.
The configured isolate limit is varied. Publication already populated disk
caches, so first-request latency is interpreter initialization with cached
artifacts, not a cold S3 download. `native_ready_ms` measures process startup
to health; `first_request_ms` starts after health. `deploy_ready_ms` includes
building and starting the development server, not production rollout latency.

The final workflow alternates variant order across three rounds and reports
medians of each metric. It compares the original and candidate Python runtime
artifacts against TypeScript on the same pinned native binary. Rebuild the
native artifact and change its recorded SHA/run ID if testing Rust changes.

Run the baseline using the `benchmark-native-python` label on the draft PR.
Run the runtime A/B with `benchmark-native-python-final` (remove/re-add to
repeat). The workflow checks concurrency/cancellation correctness before the
A/B load. Logs and machine-readable measurements are uploaded as Actions
artifacts, including partial measurements if a later sample fails.

```sh
python tools/python-runtime/build.py candidate-runtime
python tools/python-bench/measure.py \
  --binary native-bin/celld \
  --baseline-runtime native-bin/python-runtime \
  --candidate-runtime candidate-runtime \
  --variants baseline,candidate,typescript \
  --rounds 3 --seconds 10 --clients 256 --delays 10,50
```

Prerequisites for this benchmark tooling: Python, Go and esbuild (for TypeScript
publication). These are not server dependencies of deployed Python workers.

`results/2026-09-06-baseline.json` is the initial one-round diagnostic sweep,
not the final repeated A/B. Use within-run comparisons; different GitHub runs
can receive different CPU models even with the same runner label.
