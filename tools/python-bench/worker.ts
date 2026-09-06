// Same validation, JSON transformation and upstream work as worker.py.
export default {
  async fetch(request: Request, env: {UPSTREAM: string}) {
    const args = await request.json() as {name?: unknown};
    const name = args.name;
    if (typeof name !== 'string' || name.length < 1 || name.length > 128)
      return new Response('invalid name', {status:422});
    if (!env.UPSTREAM) return Response.json({result:'Hello, ' + name});
    const response = await fetch(env.UPSTREAM, {method:'POST',
      headers:{'content-type':'application/json'}, body:JSON.stringify({name})});
    if (!response.ok) throw new Error('upstream failed');
    const price = await response.json() as {customer: string; unit_price_cents: number; stock: number; currency: string};
    if (price.customer !== name || !Number.isInteger(price.unit_price_cents)
        || !Number.isInteger(price.stock) || price.stock < 2 || typeof price.currency !== 'string')
      throw new Error('invalid price');
    return Response.json({result:{customer:name, total_cents:price.unit_price_cents * 2, currency:price.currency, trace:name}});
  },
};
