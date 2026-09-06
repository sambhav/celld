# Async-only Pyodide pilot

The no-JSPI artifact improved throughput about 10–12% in a single five-second
pilot per workload, but did not approach TypeScript performance. It remains an
explicit experiment because it removes synchronous I/O via run_sync/WSGI.
The default runtime retains JSPI.

[GitHub run](https://github.com/sambhav/celld/actions/runs/34025494290), source
`dab62c045eb9d05ed4cce2b6bab39e5baf5a2c64`, AMD EPYC 7763, two physical/four
logical CPUs. Both Python arms include the request-local scheduler changes.
Same original native binary in every arm; four-isolate limit, 256 clients.

| Workload | Default req/s | Async-only req/s | TypeScript req/s |
| --- | ---: | ---: | ---: |
| JSON hello | 4,296 | 4,765 | 18,858 |
| Real HTTP I/O, 10 ms upstream | 1,923 | 2,159 | 7,290 |
| Real HTTP I/O, 50 ms upstream | 1,872 | 2,062 | 4,703 |

240,972 measured responses, zero errors, exact upstream counts. Native smoke
passed including NumPy, Pydantic, overlapping asyncio/context/cancellation
checks and confirmation that `can_run_sync()` is false. RSS remained around
1 GiB and first request after native health around 0.7 seconds.

These are pilot observations, not repeated medians or a reason to change the
default. SQLite development storage, warm artifact caches and shared CPU with
the driver/upstream apply. No S3 cold download or idle scale-to-zero wake was
measured. See `2026-09-06-async-pilot.json` for every sample and machine metadata.
