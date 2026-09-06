from workers import DurableObject, Response, WorkerEntrypoint


class Default(WorkerEntrypoint):
    async def fetch(self, request):
        counter = self.env.COUNTERS.getByName("default")
        return Response.from_json({"count": await counter.increment()})


class Counter(DurableObject):
    def __init__(self, ctx, env):
        super().__init__(ctx, env)

    async def increment(self):
        async def update(tx):
            count = (await tx.get("count") or 0) + 1
            await tx.put("count", count)
            return count
        return await self.ctx.storage.transaction(update)
