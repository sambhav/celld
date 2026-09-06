"""Real HTTP checks for generated clients, public exports and concurrent async calls."""
import concurrent.futures
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent
binary = str(Path(sys.argv[1]).resolve())
fixture = str(HERE / 'examples/functions.py')
lock = threading.Lock()
counters = {'active': 0, 'peak': 0, 'completed': 0}


class Upstream(BaseHTTPRequestHandler):
    def do_POST(self):
        name = json.loads(self.rfile.read(int(self.headers['content-length'])))['name']
        with lock:
            counters['active'] += 1
            counters['peak'] = max(counters['peak'], counters['active'])
        try:
            time.sleep(.02 if name.endswith('a') else .005)
            self.send_response(200)
            self.end_headers()
            self.wfile.write(json.dumps({'customer':name, 'unit_price_cents':1999}).encode())
        finally:
            with lock:
                counters['active'] -= 1
                counters['completed'] += 1

    def log_message(self, *args):
        pass


upstream = ThreadingHTTPServer(('127.0.0.1', 0), Upstream)
threading.Thread(target=upstream.serve_forever, daemon=True).start()
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
source = subprocess.check_output([binary, 'client', fixture], text=True)
namespace = {}
exec(compile(source, 'generated_client.py', 'exec'), namespace)
manifest = json.loads(subprocess.check_output([binary, 'inspect', fixture], text=True))
assert {f['name'] for f in manifest} == {'hello', 'quote', 'quotes', 'total'}
server = subprocess.Popen([binary, 'serve', fixture, str(port), f'http://127.0.0.1:{upstream.server_port}'])
try:
    url = f'http://127.0.0.1:{port}'
    for _ in range(300):
        if server.poll() is not None:
            raise RuntimeError('server exited')
        try:
            urllib.request.urlopen(url + '/health', timeout=1).close()
            break
        except OSError:
            time.sleep(.01)
    else:
        raise RuntimeError('server readiness timeout')
    client = namespace['Client'](url)
    assert client.hello() == 'Hello, world'
    assert client.hello(name='Ada') == 'Hello, Ada'
    assert client.total(prices=[2, 3], quantity=4) == 20
    assert client.quote(name='single') == {'customer':'single', 'total_cents':3998}
    for operation in [lambda: client.total(prices=[True]), lambda: client.hello(name=3)]:
        try:
            operation()
        except urllib.error.HTTPError as error:
            assert error.code == 422
        else:
            raise AssertionError('bad input accepted')
    for name in ['_greeting', '_fetch_json', 'json', 'missing']:
        try:
            urllib.request.urlopen(urllib.request.Request(url+'/call/'+name, data=b'{}', headers={'content-type':'application/json'}))
        except urllib.error.HTTPError as error:
            assert error.code == 404, (name,error.code)
        else:
            raise AssertionError('non-public function callable')
    def invoke(i):
        names = [f'{i}-a', f'{i}-b', f'{i}-c', f'{i}-d']
        assert client.quotes(names=names) == [{'customer':n, 'total_cents':3998} for n in names]
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(invoke, range(32)))
    assert counters['completed'] == 129 and counters['active'] == 0, counters
    assert counters['peak'] > 1, counters
    assert client.hello() == 'Hello, world'
    print('FUNCTION_CHECKS='+json.dumps({'exports':manifest,'concurrent_invocations':32,'upstream':counters,'generated_client':True,'private_functions_hidden':True,'strict_types':True}))
finally:
    server.terminate()
    server.wait(timeout=10)
    upstream.shutdown()
