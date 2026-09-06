"""Cloudflare Python entrypoint dispatch; web values cross through Pyodide FFI."""
import importlib
import inspect

from workers import Request, Response, WorkerEntrypoint


class Runtime:
    def __init__(self, module):
        source = importlib.import_module(module)
        self.entrypoint = getattr(source, "Default", None)
        if self.entrypoint is None or not issubclass(self.entrypoint, WorkerEntrypoint):
            raise TypeError("Python main must export class Default(WorkerEntrypoint)")

    async def fetch(self, request, env, ctx):
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
