def hello(name: str = "world"):
    return f"Hello, {name}!"


def increment(amount: int = 1, *, ctx):
    value = ctx.storage.get("count", 0) + amount
    ctx.storage.put("count", value)
    return value


def read(ctx):
    return ctx.storage.get("count", 0)


def schedule(ctx, delay_ms: int = 100):
    ctx.storage.set_alarm(ctx.now() + delay_ms)


def alarm(ctx):
    ctx.storage.put("alarmed", True)
