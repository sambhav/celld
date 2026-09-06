"""Every public top-level function is callable; no App or decorator required."""
import json
import asyncio


def hello(name: str = 'world') -> str:
    return _greeting(name)


def total(prices: list[int], *, quantity: int = 1) -> int:
    return sum(prices) * quantity


async def quote(name: str) -> dict:
    # Prototype host capability: one fixed upstream, supplied by the server.
    price = json.loads(await _fetch_json(name))
    return {'customer': name, 'total_cents': price['unit_price_cents'] * 2}


def _greeting(name):
    return 'Hello, ' + name


async def quotes(names: list[str]) -> list:
    return await asyncio.gather(*(quote(name) for name in names))
