"""Exercise real Pyodide scheduling, cancellation and overlapping request state."""
import asyncio
import contextvars

from workers import Response, WorkerEntrypoint

request_id = contextvars.ContextVar('request_id')


class Default(WorkerEntrypoint):
    async def fetch(self, request):
        args = await request.json()
        name = args['id']
        if args.get('async_only'):
            from pyodide.ffi import can_run_sync
            assert not can_run_sync(), 'async-only runtime enabled stack switching'
        token = request_id.set(name)
        try:
            async def child(index):
                for _ in range(80):
                    await asyncio.sleep(0)
                    assert request_id.get() == name
                await asyncio.sleep(.003)
                assert request_id.get() == name
                return index

            loop = asyncio.get_running_loop()
            cancelled = []
            handle = loop.call_soon(cancelled.append, 'should not run')
            handle.cancel()
            task = asyncio.create_task(asyncio.sleep(10))
            await asyncio.sleep(0)
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass
            else:
                raise AssertionError('task cancellation ignored')

            timer = loop.create_future()
            loop.call_later(.005, timer.set_result, True)
            values = await asyncio.gather(*(child(i) for i in range(3)))
            assert await timer
            assert not cancelled
            assert request_id.get() == name
            return Response.from_json({'id':name, 'values':values, 'greeting':self.env.GREETING})
        finally:
            request_id.reset(token)
