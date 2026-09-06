def hello(name: str = "world"):
    return f"Hello, {name}!"


async def increment(ctx, name: str = "default", amount: int = 1):
    return await ctx.object(name, binding="COUNTERS").call("increment", amount=amount)


class Counter:
    def __init__(self, ctx):
        self.ctx = ctx

    def increment(self, amount: int = 1):
        def update(storage):
            value = storage.get("count", 0) + amount
            storage.put("count", value)
            return value
        return self.ctx.storage.transaction(update)

    def schedule(self, delay_ms: int = 100):
        self.ctx.storage.set_alarm(self.ctx.now() + delay_ms)

    def alarm(self):
        self.ctx.storage.put("alarmed", True)
