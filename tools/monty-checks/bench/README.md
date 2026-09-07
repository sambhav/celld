# Native Monty and TypeScript HTTP comparison

Monty runs entirely through the native worker backend: Python artifacts, HTTP
routing and bodies, interpreter continuations, timers, HTTP client, storage and
durable method dispatch. Python workers allocate no V8 isolate. TypeScript uses
celld's usual V8 worker backend in the same binary.

## Run

```sh
CARGO_PROFILE_LAB_LTO=false CARGO_PROFILE_LAB_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo build -p celld --profile lab --locked
python tools/monty-checks/bench.py target/lab/celld --output results.json
```

Python 3.12+ on Linux runs the standard-library benchmark driver. Put esbuild
on PATH for the TypeScript build (measured with 0.25.9). Monty itself requires
neither Node/esbuild nor an installed Python interpreter to build or serve.

Both runtimes use one stateless pool slot. The test rotates runtime order and
runs three 2-second repetitions at concurrency 1 and 16, after a 0.25-second
warmup. Up to four Python client processes use keep-alive connections and check
every response. Counter replies must be unique and increasing. No build or
other test suite ran during these measurements.

The counter updates the same ID through an explicit SQLite transaction; the
TypeScript method uses synchronous storage.kv inside transactionSync. Both
methods hold the native input gate. Their storage encodings differ (JSON text
and V8 structured clone), so this compares the real interfaces as a whole.
The empty durable method measures dispatch without application storage work,
but still includes interpreter execution, serialization, routing and gates.
The fetch case POSTs a 16 KiB JSON value to the same server's echo handler and
returns its response; it measures two HTTP handlers plus an actual HTTP client
round trip. Binary buffers stay native throughout the Monty HTTP path.

## Results

**72 samples, 625,211 validated responses, zero errors.**

This is the completed rerun. An earlier attempt stopped after 49 samples when Monty's JSON echo returned HTTP 500 at concurrency 1. The old harness captured only the status; the cause remains unknown. The driver now includes error bodies and server logs when a run fails.

Median requests/second across the three repetitions:

| Concurrency | Case | Monty | TypeScript | Monty / TS |
| --- | --- | ---: | ---: | ---: |
| 1 | Text greeting | 5,322 | 5,108 | 1.04× |
| 1 | 16 KiB JSON echo | 4,418 | 2,934 | 1.51× |
| 1 | Empty durable method | 1,710 | 1,951 | 0.88× |
| 1 | Durable counter | 434 | 401 | 1.08× |
| 1 | 10 ms timer | 85 | 85 | 1.00× |
| 1 | 16 KiB HTTP fetch | 1,716 | 1,430 | 1.20× |
| 16 | Text greeting | 14,492 | 18,764 | 0.77× |
| 16 | 16 KiB JSON echo | 9,720 | 6,778 | 1.43× |
| 16 | Empty durable method | 6,814 | 9,357 | 0.73× |
| 16 | Durable counter | 2,069 | 2,552 | 0.81× |
| 16 | 10 ms timer | 1,369 | 1,364 | 1.00× |
| 16 | 16 KiB HTTP fetch | 2,986 | 2,618 | 1.14× |

At concurrency 16, median per-sample p95 latency (milliseconds):

| Case | Monty | TypeScript |
| --- | ---: | ---: |
| Text greeting | 1.52 | 1.37 |
| 16 KiB JSON echo | 2.18 | 3.27 |
| Empty durable method | 3.61 | 2.91 |
| Durable counter | 11.37 | 9.97 |
| 10 ms timer | 12.44 | 12.56 |
| 16 KiB HTTP fetch | 6.63 | 7.45 |

These are short, warm local measurements using a dev bucket. They do not
measure remote fleet durability, distributed ownership, cold startup, memory,
or streaming. The node binary still includes and initializes V8 for TypeScript
support; Python worker materialization and invocation do not enter it.
No startup or memory improvement is claimed from this throughput test.

The rankings depend on load and payload. Removing JavaScript from Monty's
execution path does not remove interpreter, serialization, routing or storage
costs. Do not attribute a difference against TypeScript to a single component.

The runner reported 9 CPUs in affinity and CPU quota `800000 100000`; affinity was not pinned. Client CPU peaked at 1.08 aggregate cores. Server CPU and RSS were not measured because process statistics are not reliable in this environment.

[Raw samples](results-native.json) include the runner timestamp and binary SHA-256:
`3b02336ee82460be14ad12af5b7934a246f94897c89b81f291c6bd7fe92597c2`.
