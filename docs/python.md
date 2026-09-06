# Python runtimes

Celld has two Python backends. Select one in `wrangler.jsonc` with
`"python_runtime": "pyodide"` or `"python_runtime": "monty"`.
Pyodide is the default for `.py` entrypoints. Monty is experimental.

| | Pyodide | Monty |
|---|---|---|
| Execution | CPython in WASM | Native Rust interpreter |
| App style | Cloudflare `WorkerEntrypoint` / `DurableObject` | Public functions or simple classes |
| Packages | Pinned Pyodide catalog, including Pydantic and NumPy | Supported Monty stdlib; no third-party imports |
| State | Celld durable-object storage | The same celld durable-object storage |
| Validation | Pydantic in Python | Strict JSON-compatible parameter annotations |
| Runtime artifacts | Separate shared interpreter, stdlib, snapshot and wheels | Interpreter linked into celld; small shared lifecycle adapter |
| Server dependencies | Celld and the fleet bucket | Celld and the fleet bucket |

The measured standalone Monty experiment motivated this backend. It does **not**
establish throughput or memory isolation of this integrated implementation.
See [benchmark methodology and historical results](../tools/monty-bench/results/2026-09-06.md).

## Monty functions

```python
def hello(name: str = "world"):
    return f"Hello, {name}!"
```

```json
{
  "name": "hello",
  "main": "worker.py",
  "python_runtime": "monty",
  "compatibility_date": "2026-09-06"
}
```

Run `celld dev` or deploy with the ordinary `celld deploy` bucket flags.
No Python, Pyodide artifact directory, Node, pip or pycelld is required by
the native build/serve path. Source changes use celld's normal reload path.

Public top-level functions are exported automatically. Private names, imported
functions, aliases and nested functions are excluded. A literal `__all__` can
restrict the function list. Inputs use named arguments, defaults and strict
`str`, `int`, `float`, `bool`, `list[T]`, `dict[str,T]`, and `T | None` annotations.
Return values must be JSON-compatible; return annotations are not validated.

A stateless class is also supported: define `class Default` with public instance
methods. If present, `Default` supplies the exported methods instead of module
functions. It may use `__init__(self, ctx)` for context injection; no base class
or decorator is required.

## Optional context and function state

Declare `ctx` only when needed. No import or decorator is required:

```python
def hello(name: str = "world"):
    return f"Hello, {name}!"

def increment(ctx, amount: int = 1):
    value = ctx.storage.get("count", 0) + amount
    ctx.storage.put("count", value)
    return value
```

Generate a standalone client with the native binary:

```sh
celld client worker.py > client.py
celld dev
```

```python
from client import Client, AsyncClient

client = Client("http://127.0.0.1:9876")  # use the address printed by celld dev
client.hello(name="Sam")                  # stateless
client["counter-1"].increment()            # durable, keyed call
client["counter-2"].increment(amount=10)   # independent state
```

`AsyncClient` offers the same calls with `await`, using Python's standard-library
thread executor for network I/O. No pycelld or Python package installation is
required. Defaults belong to the server; clients do not silently retry errors.

The compiler supplies the durable object binding and SQLite migration. Functions
within one app share a key's storage. Different keys have independent storage;
different applications have separate namespaces. A key is an identity, **not an
authorization boundary**: the embedding platform must authorize key access.

Without a key, the function stays stateless: `ctx.env`, fetch, timers and object
calls work, but storage access raises a catchable error. With a key, the whole
function runs through celld's serial object input gate. A failed call is not an
automatic transaction: use `ctx.storage.transaction(callback)` for rollback.

`async def forward(ctx, name): return await ctx.object(name).call("increment")`
calls a key in the same app. Define `alarm(ctx)` to handle scheduled alarms.
`ctx` is excluded from generated method arguments and cannot be supplied by callers.
For editor annotations, `ctx` and `from celld import Context` are accepted
by the native compiler; annotations are optional and no SDK is loaded.

See [the minimal function example](../examples/monty-functions). The Python SDK
and pycelld frontend are paused; native Monty is the active development path.

## Monty durable classes

```python
async def increment(ctx, name: str, amount: int = 1):
    return await ctx.object(name, binding="COUNTERS").call("increment", amount=amount)

class Counter:
    def __init__(self, ctx):
        self.ctx = ctx

    def increment(self, amount: int = 1):
        value = self.ctx.storage.get("count", 0) + amount
        self.ctx.storage.put("count", value)
        return value
```

Add the ordinary Durable Object binding and migration:

```json
{
  "durable_objects": {
    "bindings": [{"name": "COUNTERS", "class_name": "Counter"}]
  },
  "migrations": [{"tag": "v1", "new_sqlite_classes": ["Counter"]}]
}
```

This fragment belongs in the preceding Wrangler config. A complete runnable
project is in [examples/monty](../examples/monty).

Public instance methods become object RPC methods accepting one named-argument
object. Use `__init__(self, ctx)` or omit the constructor. Inheritance,
metaclasses, decorators, static/class methods and property descriptors are
outside this adapter's scope. A method named `alarm` is the scheduler handler,
not a public function endpoint. Other underscore methods remain internal.

Every invocation gets a fresh Python heap and class instance. Instance fields
are temporary; put persistent data in `ctx.storage`. Celld owns the object
identity, ownership epoch, input gate, SQLite database, replication, output
gate, eviction, restart and alarms. The whole Monty object invocation is
serialized using celld's input gate, including awaited I/O. Avoid cyclic
object calls: calling back into an object whose turn is still blocked can
deadlock until its event budget expires.

## Injected capabilities

Functions receive `ctx` when they declare that parameter; it is excluded from
client inputs. Classes receive it in the constructor. `Context` is a compiler
provided import, so importing the Python SDK is unnecessary inside Monty.

| API | Meaning |
|---|---|
| `ctx.id`, `ctx.name` | Durable identity; `None` for stateless calls |
| `ctx.env` | Scalar Wrangler vars |
| `ctx.storage.get(key, default=None)` | Read a JSON value |
| `put(key, value)`, `delete(key)` | Write/delete a value |
| `list(prefix="", limit=1000, reverse=False)` | Read matching keys as a dictionary |
| `delete_all()`, `sync()` | Clear storage / wait for durability |
| `sql(query, *bindings)` | Parameterized SQL, returning JSON-compatible rows |
| `transaction(callback)` | Atomic storage callback; exceptions roll back |
| `get_alarm()`, `set_alarm(timestamp_ms)`, `delete_alarm()` | Persistent scheduler state; handler metadata is in `ctx.alarm` |
| `ctx.object(name, binding="COUNTERS").call(method, **args)` | Await a named durable method |
| `await ctx.fetch(url, method="GET", headers=None, body=None)` | Fetch status, headers and text body |
| `await ctx.sleep(seconds)` | Host timer |
| `ctx.now()`, `ctx.uuid()`, `ctx.log(message)` | Host clock, UUID and logging |

Storage methods above are on `ctx.storage`. Transactions accept synchronous
Python callbacks; nested transactions use celld savepoints. External I/O is
refused inside a transaction. An unfinished transaction is rolled back when
execution fails. Individual writes outside a transaction retain ordinary
Durable Object semantics: a later handler error does not undo earlier writes.

Rust registers the host callable, checks its capability name, limits host calls,
and resumes Monty. A small JS adapter connects those calls to celld's existing
native-backed storage, alarm, fetch and object machinery. This deliberately
preserves celld's durability and lifecycle gates rather than opening a second
SQLite database or adding a separate state service.

`ctx.caller`, `ctx.client`, `ctx.call_id` and `ctx.attempt` on function endpoints
carry caller-supplied client metadata. They are untrusted application data,
not authentication. They are not automatically propagated to object RPC.
There is no durable result replay: opting into client retries can repeat effects.

Current limits: 256 KiB source, 1 MiB argument/result payloads, 1,000 host calls,
100 ms interpreter budget, 100 recursive calls, 16 nested transactions and 256
suspended sessions per isolate. The interpreter budget excludes host waiting.
Celld's event budgets still apply. Native Monty allocations are **not** isolated
by V8's heap limit; this backend is not yet suitable for mutually untrusted
tenants without a separate memory/failure containment design. JSON byte limits
do not provide an interpreter heap limit.

I/O suspension currently executes capabilities sequentially within a Python
invocation; independent worker requests can overlap. `asyncio.gather` does not
currently fan out host capabilities in this adapter. Streaming, WebSocket
hibernation, queue/workflow handlers, arbitrary module loading and CPython
extension wheels remain outside the Monty surface.

## Pyodide and Cloudflare Python

Use the ordinary `workers` SDK, including `WorkerEntrypoint.fetch` and classes
inheriting `DurableObject`. Public methods, `fetch` and `alarm` dispatch to the
Python instance. Bindings use the Cloudflare wrappers. One lazily initialized
interpreter is shared by the worker and its objects within an isolate; object
instances remain separate. Configure `python_workers` in compatibility flags.

See [the complete durable example](../examples/python-durable) and the
[runtime artifact/package guide](../examples/python/README.md). Runtime artifacts
must come from this fork's matching build; they remain separate and are reused
by content hash in S3. Queue/workflow entrypoints still require a compiler hook.

