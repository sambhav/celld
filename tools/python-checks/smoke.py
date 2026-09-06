"""Test the actual native Python dev path, validation, reload and error recovery."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='celld python ') as directory:
    root = Path(directory) / 'project'
    shutil.copytree('examples/python', root)
    subprocess.run(['pycelld', 'lock', str(root)], check=True)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = {key: value for key, value in os.environ.items() if not key.startswith('CELLD_')}
    env.update(CELLD_MAX_STATELESS_ISOLATES='1', CELLD_V8_HEAP_LIMIT_MB='256')
    log_path = Path(directory) / 'dev.log'
    with log_path.open('w') as log:
        process = subprocess.Popen([binary, 'dev', str(root), '--port', str(port)], env=env, stdout=log, stderr=log)
        def call():
            request = urllib.request.Request(f'http://127.0.0.1:{port}/hello', data=b'{"name":"Sam"}',
                headers={'Content-Type':'application/json'})
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)["result"]
        def wait_for(expected):
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError(log_path.read_text())
                try:
                    if call() == expected:
                        return
                except OSError:
                    pass
                time.sleep(.2)
            raise AssertionError(log_path.read_text())
        try:
            wait_for('Hello, Sam!')
            source = root / 'src/worker.py'
            code = source.read_text()
            source.write_text(code.replace('Hello,', 'Welcome,'))
            wait_for('Welcome, Sam!')
            source.write_text('syntax error!\n')
            deadline = time.monotonic() + 60
            while 'Python build failed' not in log_path.read_text():
                assert time.monotonic() < deadline, log_path.read_text()
                time.sleep(.2)
            assert call() == 'Welcome, Sam!'
            source.write_text(code)
            wait_for('Hello, Sam!')
            # Ordinary reads must not cause the old watcher rebuild loop.
            before = log_path.read_text().count('rebuilt')
            for _ in range(5):
                source.read_text()
                assert call() == 'Hello, Sam!'
            time.sleep(1)
            assert log_path.read_text().count('rebuilt') == before
            print('Native Python config, snapshot hello, reload, invalid-edit recovery: PASS')
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            print(log_path.read_text()[-12000:])
