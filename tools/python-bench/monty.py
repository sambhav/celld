import json

async def hello(name: str, ctx):
    if not 1 <= len(name) <= 128:
        raise ValueError('invalid name')
    response = await ctx.fetch(ctx.env['UPSTREAM'], method='POST',
        headers={'content-type':'application/json'}, body=json.dumps({'name':name}))
    if response['status'] != 200:
        raise ValueError('upstream failed')
    price = json.loads(response['body'])
    if (price.get('customer') != name or type(price.get('unit_price_cents')) is not int
        or type(price.get('stock')) is not int or price['stock'] < 2 or not isinstance(price.get('currency'),str)):
        raise ValueError('invalid price')
    return {'customer':name,'total_cents':price['unit_price_cents'] * 2,
            'currency':price['currency'],'trace':name}
