# Native Python functions

Only the celld binary is needed to build and serve this app:

```sh
celld dev examples/monty-functions
```

In another terminal, generate a client using celld's Rust compiler:

```sh
celld client examples/monty-functions/worker.py > client.py
```

The generated file uses Python's standard library. No celld package is installed:

```python
from client import Client

client = Client("http://127.0.0.1:9876")
print(client.hello(name="Sam"))
print(client["counter-1"].increment())
print(client["counter-1"].read())
```

Or use `AsyncClient` and await the same methods.

`hello` needs no context. `increment` asks for `ctx` as a keyword-only parameter;
it is injected by Rust and never appears in client arguments. Calls with a key
share that key's durable storage and execute serially. The compiler supplies the
object class, binding and migration. Keys are scoped to this application.

Edit `worker.py` while `celld dev` is running to reload it. Counters survive reload
and server restart. `alarm(ctx)` is a scheduler handler, excluded from the public
client. Classes are available when useful, but are not required for state.

A stateless `client.increment()` fails because it has no storage identity.
Use explicit storage transactions for rollback; errors do not undo prior writes
by default. Keys identify objects and do not authenticate callers.

Deploy through the ordinary `celld deploy` command and fleet-bucket options.
See [the runtime guide](../../docs/python.md) for supported capabilities and limits.
