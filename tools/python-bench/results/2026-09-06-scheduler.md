# Request-local scheduler A/B — 2026-09-06

The scheduler and cached dispatch improve warm throughput, but do not deliver TypeScript parity. This run checked **2,758,268 measured responses with zero errors**; every measured outbound I/O request also matched the upstream completion counters.

[GitHub run](https://github.com/sambhav/celld/actions/runs/34024371368), source `fe0ca8f7dcd4f4afa48bbbcfe8ebdc04887fceb2`. Native binary source `37e41ff2dcd6e1097f4207406a6b5a63a8e647fc`, SHA-256 `e0a05b06cbb86b9f9c922a40e198f41193ecc52af3c12086f57d07f3459c71ed`. AMD EPYC 9V74 runner with **2 physical cores / 4 logical CPUs**. Driver, upstream and server share that CPU budget.

Three fresh-process rounds per combination, 10 seconds per measurement, 256 concurrent clients. Values below are medians. Both Python arms use the same binary and WASM interpreter. Baseline is the original runtime; candidate adds cached dispatch, omitted empty hooks, and bounded asyncio microtasks with request-local scheduling state.

| Workload | Isolate limit | Original Python req/s | Scheduler req/s | TypeScript req/s | Python gain | Python / TS |
|---|---:|---:|---:|---:|---:|---:|
| JSON hello | 1 | 1,742 | 2,105 | 17,032 | +20.9% | 12.4% |
| HTTP I/O, 10 ms upstream | 1 | 824 | 948 | 4,429 | +15.1% | 21.4% |
| HTTP I/O, 50 ms upstream | 1 | 780 | 926 | 4,371 | +18.8% | 21.2% |
| JSON hello | 4 | 4,574 | 5,081 | 26,016 | +11.1% | 19.5% |
| HTTP I/O, 10 ms upstream | 4 | 2,089 | 2,341 | 9,287 | +12.1% | 25.2% |
| HTTP I/O, 50 ms upstream | 4 | 2,029 | 2,283 | 4,786 | +12.5% | 47.7% |

At four isolates, hello p95 latency increased from 73.9 to 86.9 ms despite higher throughput; the scheduler does not improve every latency metric. I/O p95 improved from 145.7 to 137.9 ms (10 ms upstream) and from 154.4 to 137.4 ms (50 ms upstream).

Changing the candidate isolate limit from one to four gives 2.41× hello throughput, 2.47× I/O at 10 ms, and 2.46× I/O at 50 ms. It is not linear scaling; the runner has only two physical cores and also runs the load generator/upstream.

After node health, first hello request medians remain about 0.62–0.64 seconds for Python versus 1.4 ms for TypeScript. These are fresh interpreter starts with cached artifacts, not cold S3 downloads or measured idle eviction/wake cycles. Node health time is separate and varied between about 1.3 and 4.3 seconds for Python due to the local development-store readiness lifecycle.

Four-isolate candidate RSS medians were 952 MiB for hello, 976 MiB for 10 ms I/O, and 1,053 MiB for 50 ms I/O. Memory changes across the two arms were inconsistent; no general RSS reduction is claimed.

[Raw measurements](2026-09-06-scheduler.json) include latency distributions, first-request/readiness metrics, driver CPU, memory and upstream counts. [Methodology](../README.md) explains the local SQLite store, warmed disk caches and response checks. This is the built-in Cloudflare Python backend, not the separate decorator/Pydantic SDK.

Correctness: [native build/test run](https://github.com/sambhav/celld/actions/runs/34024363125) passed the Rust build, packing/compiler/artifact checks, 12 JS regression tests, real pinned-Pyodide checks, and native smoke tests for reload, invalid-edit recovery, Pydantic, NumPy, shared artifacts and concurrent asyncio context/cancellation/timers.

