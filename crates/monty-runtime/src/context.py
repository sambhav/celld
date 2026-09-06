"""Small Python facade; capabilities are registered and checked by Rust."""
import json as _celld_json

def _celld_call(operation, *args):
    reply = _celld_json.loads(_celld_host(operation, _celld_json.dumps(args)))
    if "error" in reply:
        raise RuntimeError(reply["error"])
    return reply.get("result")

class _CelldStorage:
    def get(self, key, default=None):
        value = _celld_call("storage.get", key)
        return default if value is None else value
    def put(self, key, value):
        return _celld_call("storage.put", key, value)
    def delete(self, key):
        return _celld_call("storage.delete", key)
    def list(self, prefix="", limit=1000, reverse=False):
        return _celld_call("storage.list", prefix, limit, reverse)
    def delete_all(self):
        return _celld_call("storage.delete_all")
    def sql(self, query, *bindings):
        return _celld_call("storage.sql", query, list(bindings))
    def sync(self):
        return _celld_call("storage.sync")
    def get_alarm(self):
        return _celld_call("storage.get_alarm")
    def set_alarm(self, timestamp_ms):
        return _celld_call("storage.set_alarm", timestamp_ms)
    def delete_alarm(self):
        return _celld_call("storage.delete_alarm")
    def transaction(self, callback):
        _celld_call("storage.transaction_begin")
        try:
            result = callback(self)
        except BaseException:
            _celld_call("storage.transaction_rollback")
            raise
        _celld_call("storage.transaction_commit")
        return result

class _CelldObject:
    def __init__(self, binding, name):
        self.binding = binding
        self.name = name
    async def call(self, function, **args):
        return _celld_call("object.call", self.binding, self.name, function, args)

class Context:
    def __init__(self, metadata):
        self.id = metadata.get("id")
        self.name = metadata.get("name")
        self.env = metadata.get("env", {})
        self.storage = _CelldStorage()
        self.caller = metadata.get("caller", {})
        self.client = metadata.get("client", {})
        self.call_id = metadata.get("call_id")
        self.attempt = metadata.get("attempt", 1)
    def object(self, binding, name):
        return _CelldObject(binding, name)
    async def fetch(self, url, method="GET", headers=None, body=None):
        return _celld_call("fetch", url, method, headers or {}, body)
    async def sleep(self, seconds):
        return _celld_call("sleep", seconds)
    def now(self):
        return _celld_call("now")
    def uuid(self):
        return _celld_call("uuid")
    def log(self, message):
        return _celld_call("log", message)

_celld_context = Context(_celld_json.loads(_celld_metadata))
