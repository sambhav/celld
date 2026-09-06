"""Native Monty end-to-end checks. Python stdlib only; no pycelld package."""
import asyncio
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from urllib.request import Request, urlopen
from urllib.error import HTTPError

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    project = root / 'app'
    shutil.copytree('examples/monty-functions', project)
    source = project / 'worker.py'
    source.write_text(source.read_text() + '''
def metadata(*, ctx):
    return {'name':ctx.name, 'caller':ctx.caller, 'alarmed':ctx.storage.get('alarmed',False)}
def rollback(ctx):
    def fail(tx):
        tx.put('count', -100)
        raise ValueError('rollback')
    try:
        ctx.storage.transaction(fail)
    except ValueError:
        return ctx.storage.get('count')
def sql(ctx):
    ctx.storage.sql('CREATE TABLE IF NOT EXISTS test (value TEXT)')
    ctx.storage.sql('INSERT INTO test VALUES (?)', 'ok')
    return ctx.storage.sql('SELECT value FROM test')
async def forward(name:str, ctx):
    return await ctx.object(name).call('increment')
def _private(): return 'secret'
''')
    generated = subprocess.check_output([binary, 'client', str(source)], text=True)
    client_path = root / 'client.py'; client_path.write_text(generated)
    spec = importlib.util.spec_from_file_location('native_client', client_path)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0)); port = sock.getsockname()[1]
    env = {k:v for k,v in os.environ.items() if not k.startswith('CELLD_')}
    env.update(CELLD_MODULE_CACHE=str(root/'cache'),CELLD_MAX_STATELESS_ISOLATES='2')
    log_path = root / 'server.log'
    client = module.Client(f'http://127.0.0.1:{port}')
    key = 'counter/東京'
    def start(log):
        process = subprocess.Popen([binary,'dev',str(project),'--port',str(port),'--logs'],env=env,stdout=log,stderr=log)
        deadline = time.monotonic()+90
        while time.monotonic()<deadline:
            if process.poll() is not None: raise AssertionError(log_path.read_text())
            try:
                with socket.create_connection(('127.0.0.1',port),timeout=.2): return process
            except OSError: time.sleep(.1)
        process.kill(); process.wait(); raise AssertionError(log_path.read_text())
    def stop(process):
        process.terminate()
        try: process.wait(timeout=15)
        except subprocess.TimeoutExpired: process.kill(); process.wait()
    with log_path.open('w') as log:
        process = start(log)
        try:
            assert client.hello() == 'Hello, world!'
            assert asyncio.run(module.AsyncClient(client._celld_url).hello(name='async')) == 'Hello, async!'
            assert client[key].increment() == 1
            with ThreadPoolExecutor(max_workers=8) as pool:
                values = list(pool.map(lambda _:client[key].increment(),range(32)))
            assert sorted(values) == list(range(2,34)), values
            assert client[key].read() == 33
            assert client['other'].increment(amount=10) == 10
            assert client[key].rollback() == 33
            assert client[key].sql() == [{'value':'ok'}]
            assert client.forward(name='forwarded') == 1
            assert client['forwarded'].read() == 1
            assert client[key].metadata()['name'] == key
            client[key].schedule()
            deadline = time.monotonic()+15
            while not client[key].metadata()['alarmed']:
                assert time.monotonic()<deadline,'alarm did not fire'
                time.sleep(.1)
            for name, args in [('increment',{}),('hello',{'ctx':{}}),('_private',{})]:
                try: client._celld_call(name,args)
                except module.CallError as e: assert e.status in (404,422)
                else: raise AssertionError('invalid call accepted: '+name)
            with urlopen(client._celld_url+'/__celld/schema') as response: schema=json.load(response)
            assert schema['functions']['increment']['stateful']=='optional'
            assert 'ctx' not in schema['functions']['increment']['arguments']['properties']
            assert 'alarm' not in schema['functions']
            # Reload changes the code without changing the key's durable identity.
            source.write_text(source.read_text().replace('Hello, {name}!', 'Welcome, {name}!'))
            deadline=time.monotonic()+45
            while True:
                try:
                    if client.hello() == 'Welcome, world!': break
                except (OSError,module.CallError): pass
                assert time.monotonic()<deadline,'reload did not apply'
                time.sleep(.2)
            assert client[key].read() == 33
            stop(process); process=start(log)
            assert client[key].increment() == 34
            print('PASS: native generated clients, optional context, keyed state, isolation, concurrency, RPC, SQL, rollback, alarms, reload and restart',flush=True)
        except BaseException:
            print(log_path.read_text(),flush=True); raise
        finally: stop(process)
