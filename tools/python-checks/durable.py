"""Exercise both Python runtimes through actual celld dev, including disk recovery."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from urllib.request import Request, urlopen
from urllib.error import HTTPError

binary = str(Path(sys.argv[1]).resolve())

def check(runtime):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory) / 'app'
        shutil.copytree('examples/monty' if runtime == 'monty' else 'examples/python-durable', root)
        if runtime == 'monty':
            with (root / 'worker.py').open('a') as f:
                f.write('''
async def inspect(ctx, name:str='default'):
    return await ctx.object('COUNTERS',name).call('inspect')
async def schedule(ctx):
    return await ctx.object('COUNTERS','default').call('schedule')
async def rollback(ctx):
    return await ctx.object('COUNTERS','default').call('rollback')
async def sql(ctx):
    return await ctx.object('COUNTERS','default').call('sql')
''')
            source=(root/'worker.py').read_text().replace('    def schedule(self,', '''    def sql(self):
        storage=self.ctx.storage
        storage.sql('CREATE TABLE IF NOT EXISTS test (id INTEGER PRIMARY KEY, value TEXT)')
        storage.sql('INSERT OR REPLACE INTO test VALUES (?,?)', 1, 'works')
        return storage.sql('SELECT value FROM test WHERE id=?', 1)
    def inspect(self):
        return {'count':self.ctx.storage.get('count',0), 'alarmed':self.ctx.storage.get('alarmed',False), 'keys':self.ctx.storage.list()}
    def rollback(self):
        def fail(tx):
            tx.put('count',-999)
            raise ValueError('rollback')
        try:
            self.ctx.storage.transaction(fail)
        except ValueError:
            return self.ctx.storage.get('count',0)
    def schedule(self,''')
            (root/'worker.py').write_text(source)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
        env={k:v for k,v in os.environ.items() if not k.startswith('CELLD_')}
        env['CELLD_MODULE_CACHE']=str(Path(directory)/'cache')
        env['CELLD_MAX_STATELESS_ISOLATES']='2'
        if os.environ.get('CELLD_PYTHON_RUNTIME'): env['CELLD_PYTHON_RUNTIME']=os.environ['CELLD_PYTHON_RUNTIME']
        log_path=Path(directory)/'server.log'
        def call(name='increment',args=None):
            path='/call/'+name if runtime=='monty' else '/'
            req=Request(f'http://127.0.0.1:{port}{path}',data=json.dumps(args or {}).encode(),headers={'content-type':'application/json'})
            try:
                with urlopen(req,timeout=30) as response: data=json.load(response)
            except HTTPError as error:
                print(f'{runtime} {name}: {error.code}: {error.read().decode()}',flush=True)
                raise
            return data['result'] if runtime=='monty' else data['count']
        def start(log):
            process=subprocess.Popen([binary,'dev',str(root),'--port',str(port),'--logs'],env=env,stdout=log,stderr=log)
            deadline=time.monotonic()+120
            while time.monotonic()<deadline:
                if process.poll() is not None: raise AssertionError(log_path.read_text())
                try:
                    # TCP readiness does not mutate the object under test.
                    with socket.create_connection(('127.0.0.1',port),timeout=.2): return process
                except OSError: time.sleep(.2)
            process.terminate()
            raise AssertionError(log_path.read_text())
        def stop(process):
            process.terminate()
            try: process.wait(timeout=15)
            except subprocess.TimeoutExpired: process.kill(); process.wait()
        with log_path.open('w') as log:
            process=start(log)
            try:
                assert call()==1
                with ThreadPoolExecutor(max_workers=8) as executor:
                    values=list(executor.map(lambda _:call(),range(16)))
                assert sorted(values)==list(range(2,18)),values
                if runtime=='monty':
                    assert call('hello')=='Hello, world!'
                    from celld.client import Client, AsyncClient
                    from celld.codegen import generate
                    import asyncio
                    import importlib.util
                    client=Client(f'http://127.0.0.1:{port}')
                    contract=client.describe()
                    assert contract['runtime']=='monty'
                    assert 'ctx' not in contract['functions']['increment']['arguments']['properties']
                    generated=generate(contract,Path(directory)/'client.py')
                    spec=importlib.util.spec_from_file_location('monty_generated_client',generated)
                    module=importlib.util.module_from_spec(spec)
                    sys.modules[spec.name]=module
                    spec.loader.exec_module(module)
                    assert module.Client(client.endpoint).hello(name='Sam')=='Hello, Sam!'
                    assert asyncio.run(module.AsyncClient(client.endpoint).hello(name='Async'))=='Hello, Async!'
                    assert client.with_context({'actor':'test'}).call('hello')=='Hello, world!' 
                    assert call('increment',{'name':'other','amount':10})==10
                    assert call('rollback')==17
                    assert call('sql')==[{'value':'works'}]
                    assert call('inspect')['count']==17
                    call('schedule')
                    deadline=time.monotonic()+15
                    while not call('inspect')['alarmed']:
                        assert time.monotonic()<deadline,'alarm did not fire'
                        time.sleep(.1)
                    try: call('_private')
                    except HTTPError as error: assert error.code==404
                    else: raise AssertionError('private function exposed')
                stop(process)
                process=start(log)
                assert call()==18,'durable state did not survive restart'
                print(runtime+': concurrent durable methods, persistence/restart'+(', isolation, transactions/rollback, alarms, public functions/classes: PASS' if runtime=='monty' else ': PASS'),flush=True)
            except BaseException:
                print(log_path.read_text(),flush=True)
                raise
            finally: stop(process)

for runtime in sys.argv[2:] or ['monty','pyodide']:
    check(runtime)
