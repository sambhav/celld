# Native Monty: working functions, fast wake-up, incomplete throughput parity

The integrated Monty backend is useful for small Python workers: it runs ordinary
functions with optional context, supports durable state without wrapper classes,
and wakes an evicted isolate in milliseconds. It is not yet TypeScript-speed on
these workloads. The native implementation is approximately 1.9× Pyodide on hello
and 1.6× on real HTTP I/O at four isolates, while using substantially less memory.

## Evidence

- [Correctness run](https://github.com/sambhav/celld/actions/runs/34055908849): native source `24806e263f09014bcb778428348ab9141cae040c`.
- [Same-runner comparison](https://github.com/sambhav/celld/actions/runs/34056837315): the same native binary and source for every backend; executable hash is in the raw report.
- [Raw measurements](2026-09-06-native-monty.json): 72 samples, **2,503,100 validated responses, zero errors**. Upstream request/completion counts match measured I/O responses.
- AMD EPYC 9V74 runner, **two physical cores / four logical CPUs**. The load driver and upstream fixture share those CPUs with celld.
- Two five-second rounds per configuration, 32 and 128 concurrent clients, 1/2/4-isolate limits. Backend order reverses between rounds.

Correctness covers optional/keyword-only context, public functions and classes,
strict JSON arguments, rejected caller-supplied `ctx`, generated sync/async clients,
keyed storage, concurrent increments, independent keys, object calls, SQL,
transaction rollback, alarms, source reload, and persistence across restart.
The native build and checks do not install pycelld or the Python SDK.

## Throughput and scaling

Median requests/second at **128 concurrent clients**:

| Workload | Isolate limit | TypeScript | Native Monty | Pyodide |
|---|---:|---:|---:|---:|
| Validated JSON hello | 1 | 16,549 | 4,439 | 2,385 |
| Validated JSON hello | 2 | 25,721 | 7,765 | 3,988 |
| Validated JSON hello | 4 | 23,932 | 10,909 | 5,782 |
| Real upstream HTTP, 10 ms | 1 | 4,407 | 1,644 | 1,034 |
| Real upstream HTTP, 10 ms | 2 | 7,531 | 2,767 | 1,714 |
| Real upstream HTTP, 10 ms | 4 | 7,974 | 4,075 | 2,488 |

Server logs confirm that the 128-client samples actually reached the configured
1, 2 and 4 live isolates in every runtime and round. These are not merely requested
limits. Monty scales **2.46× for hello and 2.48× for I/O** between one and four
isolates, not 4×. The runner has only two physical cores, and shared driver/upstream
CPU limits this experiment. It does not prove linear scaling on a larger fleet.

At four isolates and 128 clients, hello p95 is 20.33 ms for Monty versus 11.09 ms
for TypeScript and 36.83 ms for Pyodide. I/O p95 is 39.91, 20.69 and 69.26 ms,
respectively. At 32 clients Monty hello p95 is 5.18 ms, illustrating the throughput/
queueing tradeoff. These are exploratory measurements, not an equivalence test or
an exhaustive search for maximum sustainable throughput.

The I/O worker makes a real request to a controlled price service, validates its
JSON reply, computes a total and returns a unique caller trace. It does not replace
I/O with a sleep inside the interpreter. The 32-client I/O cases are also limited
by the 3,200 req/s theoretical concurrency/delay ceiling, before other costs.

## Startup, idle eviction and memory

| Metric | TypeScript | Native Monty | Pyodide |
|---|---:|---:|---:|
| First hello after fresh-process health, median at 4 isolates / 128 clients | 1.51 ms | 2.70 ms | 660.72 ms |
| First hello after verified idle isolate eviction, one probe each | 8.02 ms | 9.27 ms | 487.57 ms |
| Process RSS after hello load, 4 isolates / 128 clients | 188.5 MiB | 190.4 MiB | 992.5 MiB |
| Process RSS after I/O load, 4 isolates / 128 clients | 232.1 MiB | 244.0 MiB | 1,008.9 MiB |

The idle probe waits for all started isolates to be logged as freed, then checks
that the next request starts a new isolate and returns the correct response.
Time from the end of load to zero was approximately 20–24 seconds, dependent on
the phase of celld's 30-second maintenance tick. This is **worker-isolate scale to
zero while celld remains running**, not scale to zero of the host process.
The wake numbers are single observations and do not establish tail latency.

Fresh-process health readiness is recorded separately: roughly 1–4.3 seconds in
this run, influenced by celld fleet-settling timing. First-request numbers above
do not include that interval. Disk/artifact caches are warm; this does not measure
uncached network downloads, remote S3 durability latency, or fleet-wide rollout.
The backing store is celld's local SQLite development object store.

## Routing control: automatic state is not the throughput bottleneck

A [second same-runner experiment](https://github.com/sambhav/celld/actions/runs/34057704982)
kept the native binary, interpreter, Python function and JavaScript adapter fixed,
but manually bundled one control without any durable-object class. This checks
whether supplying optional state makes ordinary function calls unnecessarily slow.

Native source remains `24806e2`; harness source is
`b37f320d1dc5db8390110622af9745f4a8770b02`. This runner was an AMD EPYC 7763 with two
physical/four logical CPUs, so compare arms **within this run**, not its absolute
numbers against the first runner. Two five-second rounds, 128 clients, 12 samples,
**578,765 validated responses, zero errors**.
[Raw control measurements](2026-09-06-native-monty-routing.json).

| Isolate limit | Monty with automatic state | Monty without any durable class | TypeScript control |
|---:|---:|---:|---:|
| 1 | 4,046 req/s | 4,041 req/s | 11,380 req/s |
| 4 | 9,662 req/s | 9,691 req/s | 18,946 req/s |

The Monty difference is below 0.3%, not evidence of a throughput improvement.
Keep automatic state provisioning: requiring wrapper classes or another setting
would add user work without a measured benefit. The remaining interpreter/bridge
cost needs profiling; this experiment does not identify which of them dominates.

## Remaining limits

Monty supports a Python subset and does not import CPython/Pyodide wheels.
Its native allocations are outside V8's heap limit; memory/failure containment is
still required before mutually untrusted tenancy. Host calls within one Python
invocation are sequential. The current interface has no streaming, WebSocket
hibernation, queue/workflow handlers, or durable result replay. Explicit storage
transactions provide rollback; an ordinary failed call does not undo prior writes.

The standalone Monty prototype used a different host and an older/different CPU.
Its numbers must not be substituted for this integrated backend's measurements.
