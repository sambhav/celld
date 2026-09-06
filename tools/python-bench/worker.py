import json
from workers import Response, WorkerEntrypoint, fetch

class Default(WorkerEntrypoint):
    async def fetch(self, request):
        args = await request.json()
        name = args.get('name')
        if not isinstance(name, str) or not 1 <= len(name) <= 128:
            return Response('invalid name', status=422)
        if not self.env.UPSTREAM:
            return Response.from_json({'result': 'Hello, ' + name})
        response = await fetch(self.env.UPSTREAM, method='POST',
            headers={'content-type':'application/json'}, body=json.dumps({'name':name}))
        if not response.ok:
            raise ValueError('upstream failed')
        price = await response.json()
        if (price.get('customer') != name or type(price.get('unit_price_cents')) is not int
            or type(price.get('stock')) is not int or price['stock'] < 2 or not isinstance(price.get('currency'), str)):
            raise ValueError('invalid price')
        return Response.from_json({'result': {'customer':name,
            'total_cents':price['unit_price_cents'] * 2, 'currency':price['currency'], 'trace':name}})
