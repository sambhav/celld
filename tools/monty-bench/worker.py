# Same JSON parsing, validation, transformation and real upstream as native CF workers.
import json


def function(fn):
    return fn


@function
def hello(name: str) -> str:
    return 'Hello, ' + name


def handle(raw: str, io: bool) -> str:
    args = json.loads(raw)
    name = args.get('name')
    if not isinstance(name, str) or not 1 <= len(name) <= 128:
        raise ValueError('invalid name')
    if not io:
        return json.dumps({'result': hello(name)})
    # The interpreter suspends; Rust awaits real HTTP without blocking a thread.
    price = json.loads(get_price(json.dumps({'name': name})))
    if (price.get('customer') != name or type(price.get('unit_price_cents')) is not int
        or type(price.get('stock')) is not int or price['stock'] < 2
        or not isinstance(price.get('currency'), str)):
        raise ValueError('invalid price')
    return json.dumps({'result': {'customer': name, 'total_cents': price['unit_price_cents'] * 2,
                                 'currency': price['currency'], 'trace': name}})
