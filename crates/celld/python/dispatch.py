"""Cloudflare Python entrypoint dispatch; web values cross through Pyodide FFI."""
import importlib
import inspect

from workers import Request, Response, WorkerEntrypoint, DurableObject
from workers.rpc import python_from_rpc, python_to_rpc


class Runtime:
    def __init__(self, module):
        source = importlib.import_module(module)
        self.source = source
        self.entrypoint = getattr(source, "Default", None)
        if self.entrypoint is not None and not issubclass(self.entrypoint, WorkerEntrypoint):
            raise TypeError("Python main must export class Default(WorkerEntrypoint)")

    async def fetch(self, request, env, ctx):
        if self.entrypoint is None:
            raise TypeError("Python main must export class Default(WorkerEntrypoint)")
        worker = self.entrypoint(ctx, env)
        result = worker.fetch(Request(request))
        if inspect.isawaitable(result):
            result = await result
        if isinstance(result, Response):
            return result.js_object
        # Response.redirect returns the native JS Response in the upstream SDK.
        if getattr(getattr(result, "constructor", None), "name", None) == "Response":
            return result
        raise TypeError("WorkerEntrypoint.fetch must return workers.Response")

    def construct(self, name, ctx, env):
        cls = getattr(self.source, name)
        if not issubclass(cls, DurableObject):
            raise TypeError(f"{name} must inherit workers.DurableObject")
        return DurableInstance(cls(ctx, env))


class DurableInstance:
    def __init__(self, instance):
        self.instance = instance

    async def call(self, name, args):
        if name.startswith("_"):
            raise TypeError("private methods are not exported")
        values = [python_from_rpc(value) for value in args]
        handler = getattr(self.instance, name)
        # Celld supplies alarm metadata. Cloudflare Python handlers commonly
        # take no arguments; handlers declaring a parameter receive the info.
        if name == "alarm" and not inspect.signature(handler).parameters:
            values = []
        result = handler(*values)
        if inspect.isawaitable(result):
            result = await result
        if isinstance(result, Response):
            return result.js_object
        return python_to_rpc(result)
