# Public Python functions on Monty

This working prototype uses the existing Monty interpreter without a fork.
It is a standalone test server, not yet a celld deployment backend. The core
export discovery, argument validation and invocation adapter are Rust.

An app can be just `app.py`:

```python
def hello(name: str = 'world') -> str:
    return 'Hello, ' + name
```

Every public top-level function in that file is exposed automatically. No
`App`, decorator or HTTP types are needed. Private helpers begin with `_`.
Imported names, class methods, nested functions, aliases and conditional function
declarations are not discovered. A plain literal `__all__ = ['hello']` can
restrict the exports; it cannot export private or imported functions. Dynamic
or annotated `__all__` definitions are rejected. Export discovery parses source
without executing it, so it does not depend on Monty's missing `__name__` or
`__annotations__` reflection. Function decorators can still run inside Monty,
subject to its supported language subset.

Run from the repository root:

```sh
cargo build --release --locked --manifest-path tools/monty-bench/Cargo.toml --bins
# Inspect operations and their input schemas.
tools/monty-bench/target/release/monty-functions inspect app.py
# Serve locally.
tools/monty-bench/target/release/monty-functions serve app.py 8000
# Generate a normal Python client; the client needs only the standard library.
tools/monty-bench/target/release/monty-functions client app.py > client.py
```

Then call it:

```python
from client import Client

client = Client('http://127.0.0.1:8000')
assert client.hello() == 'Hello, world'
assert client.hello(name='Ada') == 'Hello, Ada'
```

Clients have generated named methods and argument annotations. Calls use keyword
arguments; optional arguments are omitted from the payload so the server owns
its defaults. A supplied `None` is sent explicitly. Input schemas reject unknown
arguments, missing required values and wrong JSON types. Supported annotations
are `str`, `int`, `float`, `bool`, `list[T]`, `dict[str, T]` and `T | None`, recursively.
Bare `list`/`dict` and unannotated arguments accept arbitrary JSON values inside
their declared shape. Integers reject booleans and strings; no Pydantic coercion
is implied. String/forward-reference annotations, custom model annotations,
positional-only parameters and variadic parameters are rejected. Return values
must be JSON serializable; output annotations are not validated. `_celld_`
parameter names are reserved for generated plumbing.

Each call starts a fresh Python heap from cached compiled code. Module globals
and mutable defaults therefore do not persist across calls. Stateful functions
were separately tested through MontyRepl and snapshot/restore; the HTTP prototype
does not yet expose persistent actors or state storage.

## Async and host I/O

`examples/functions.py` also tests asynchronous functions and `asyncio.gather`.
The private `_fetch_json(name)` capability performs a real HTTP request to one
fixed upstream address configured when starting the test server. It returns a
JSON string to Python, where `json.loads` parses it. Request code does not choose
an arbitrary upstream URL. Host futures run concurrently and are correlated by
call ID; completion order need not match call order.

This is a test capability rather than the final context/bindings API. The server
limits calls to 64 host operations, a two-second interpreter duration budget and
a five-second async driver timeout, with a two-second HTTP client timeout. Dropping
the driver cancels its outstanding host tasks. Exceptions, invalid arguments and
host failures currently map to errors in the simple development transport.
The prototype is fixed-code, in-process testing infrastructure without a
production crash-isolation boundary or allocator-backed memory limit.

## Tests

`cargo test` covers discovery, explicit exports, dynamic-export rejection,
defaults/keyword-only arguments, strict and nested JSON types, async suspension,
private operation exclusion, and fresh globals. Existing adapter tests cover
out-of-order resumes and invalid upstream responses.

`check-functions.py` generates and executes the client against the real Rust
server. It checks private/imported operation 404s, invalid-type errors and 32
concurrent calls each gathering four upstream requests; all 129 upstream calls
including the initial single request must complete with correct results.

`monty-compat` tests imports, inheritance, metaclasses, annotation reflection,
dataclasses, stateful function snapshot/restore and alternatives to the failing
warm function API. The feed API's repeated-call timing and snapshot growth are
recorded together; working once does not establish long-lived-worker suitability.
`audit-wheel.py` verifies and inspects the real Pydantic-core WASM wheel.
The `check-monty-functions` PR label runs these checks on GitHub and uploads logs.

No throughput claim from the earlier fixed-handler benchmark is transferred to
this new public-function adapter without measuring it.

## Measured compatibility follow-up

[GitHub run 34030580413](https://github.com/sambhav/celld/actions/runs/34030580413)
passed all 10 Rust tests, the generated-client HTTP checks and the wheel audit.
[Raw output](results/2026-09-06-functions.json) records source
`396ad3a3e552ed69e7ac366b1172351782ed29e9`, export schemas, compatibility failures,
all WASM import names and the following observations:

- The 32 concurrent gather invocations completed all 129 upstream requests,
  with 15 simultaneously active at peak and zero left active.
- The counter function restored from a 454-byte snapshot and continued from
  2 to 5. This is an interpreter-state test, not S3-backed actor integration.
- The minimal `call_function` builtin-lookup failure reproduces, while
  `feed_run("hello(name)", inputs=...)` works for the same function.
- In five batches of 1,000 calls, the full hello handler's feed path took
  41.35, 38.55, 38.61, 39.16 and 38.83 µs/call, including result decoding/checks.
  Its serialized session grew from 2,916 bytes to 59,738, 118,738, 177,738,
  236,738 and 295,738 bytes. Retained session history is growing across calls.
  These are diagnostic measurements, not an RSS measurement or throughput
  comparison. This run did not record CPU metadata; do not compare absolute
  timings against earlier runners. The automatic-export adapter uses fresh
  heaps and does not use this growing REPL feed path.

The next useful Monty work is a correct, reusable invocation API with bounded
call-history retention and a source-module loader. Full CPython extension
compatibility is a substantially larger undertaking.

## What Pydantic imports would require

There are three different kinds of dependency:

| Dependency | Work needed in Monty |
| --- | --- |
| Local `.py` module using supported syntax | A source-module resolver, module namespaces/cache, import/circular-import semantics and packaged-source lookup; not currently provided by the fixed built-in importer. |
| WASM library with an ABI designed for the host | A declared host capability, WASM instantiation and value/error/lifetime conversion. This can avoid CPython if the library itself does. |
| Existing Pyodide CPython extension wheel | Its CPython ABI and object/runtime semantics, Emscripten dynamic-linking environment, memory/table/allocator conventions and dependency loader; loading WASM bytes is only a small part. |

The pinned Pydantic-core 2.41.5 wheel is
`pydantic_core-2.41.5-cp314-cp314-pyemscripten_2026_0_wasm32.whl`, SHA-256
`9802bd7a5e6679ec4c13be778f19f021e6147459c89cc916b913f7970f86dddb`.
Its 4,275,289-byte WASM extension imports 202 symbols, including **165 names
beginning with `Py` or `_Py`**, and exports `PyInit__pydantic_core`. Examples are
`PyList_New`, `_Py_Dealloc`, `PyDict_SetItem`, `PyObject_GetAttr`,
`PyErr_GivenExceptionMatches` and `PyInterpreterState_Get`.
These require real CPython-compatible behavior, not no-op linker stubs.

Pydantic also has a Python model-definition layer. Even with a native core,
`class Model(BaseModel)` requires inheritance, metaclasses and runtime annotation
inspection that these Monty probes reject. Its transitive imports and custom
Python validators add further compatibility requirements. The current Rust
[pydantic-core crate](https://github.com/pydantic/pydantic/blob/main/pydantic-core/Cargo.toml)
also depends on PyO3; being written in Rust does not make it a standalone Monty
validation library. The actual pinned WASM wheel audit gives concrete evidence
for the deployed Pyodide version, independently of changes to current main.

Practical choices:

1. Keep full Pydantic and declared Pyodide wheels on the Pyodide backend.
2. For Monty, generate static operation/type metadata and use a clearly specified
   JSON validation subset. This prototype does that for basic annotations.
   Supporting Pydantic-generated JSON Schema would still not reproduce arbitrary
   Pydantic validators, serializers, coercion or error semantics.
3. If richer libraries are needed occasionally, expose coarse host operations
   backed by Pyodide (for example, validate a whole payload). Values cross as
   JSON/bytes or explicit handles. This avoids implementing CPython inside Monty,
   but adds bridge latency and needs a Pyodide runtime when used. It was not
   implemented or benchmarked here, and does not make BaseModel declarations
   transparently importable in Monty.
4. Porting the full existing wheels directly would be a substantial CPython
   compatibility project. A portable validation core and a supported source-module
   loader are more targeted extension directions; neither is a small import hook.

Source references: Monty's pinned [module limitations](https://github.com/pydantic/monty/blob/af272c3116e2525249103f960b79086fd250bcef/docs/limitations/modules.md),
[class limitations](https://github.com/pydantic/monty/blob/af272c3116e2525249103f960b79086fd250bcef/docs/limitations/classes.md),
and Pydantic's [architecture](https://pydantic.dev/docs/validation/dev/internals/architecture/).
