"""Native Cloudflare Python: run without language tools, then reload and use WASM packages."""
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.request

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='celld python ') as directory:
    root = Path(directory) / 'project'
    shutil.copytree('examples/python', root)
    traps = Path(directory) / 'forbidden-tools'
    traps.mkdir()
    marker = Path(directory) / 'external-tool-called'
    for name in ('pycelld', 'python', 'python3', 'node', 'npm', 'esbuild'):
        script = traps / name
        script.write_text(f'#!/bin/sh\necho invoked > "{marker}"\nexit 93\n')
        script.chmod(0o755)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = {key:value for key,value in os.environ.items() if not key.startswith('CELLD_')}
    env.update(PATH=str(traps)+os.pathsep+env['PATH'], CELLD_MAX_STATELESS_ISOLATES='1', CELLD_V8_HEAP_LIMIT_MB='256', CELLD_MODULE_CACHE=str(Path(directory) / 'module-cache'))
    if os.environ.get('CELLD_PYTHON_RUNTIME'):
        env['CELLD_PYTHON_RUNTIME'] = os.environ['CELLD_PYTHON_RUNTIME']
    log_path = Path(directory) / 'dev.log'
    with log_path.open('w') as log:
        process = subprocess.Popen([binary,'dev',str(root),'--port',str(port),'--logs'], env=env, stdout=log, stderr=log)
        def call(data=None):
            req = urllib.request.Request(f'http://127.0.0.1:{port}/',
                data=json.dumps(data).encode() if data is not None else None,
                headers={'Content-Type':'application/json'})
            with urllib.request.urlopen(req, timeout=60) as response:
                body = response.read().decode()
                return json.loads(body) if response.headers.get('content-type','').startswith('application/json') else body
        def wait_for(expected, data=None):
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError(log_path.read_text())
                try:
                    if call(data) == expected:
                        return
                except OSError:
                    pass
                time.sleep(.2)
            raise AssertionError(log_path.read_text())
        try:
            wait_for('Hello, Cloudflare Python!')
            def shared_objects():
                with sqlite3.connect(root / '.celld/dev/objects.sqlite3') as db:
                    return db.execute("select key, etag, length(body) from objects where key like 'modules/sha256/%' order by key").fetchall()
            first_artifacts = shared_objects()
            assert len(first_artifacts) == 4, first_artifacts
            source = root / 'src/worker.py'
            code = source.read_text()
            source.write_text(code.replace('Hello,', 'Welcome,'))
            wait_for('Welcome, Cloudflare Python!')
            assert shared_objects() == first_artifacts, 'source edit uploaded runtime artifacts again'
            source.write_text('syntax error!\n')
            deadline = time.monotonic() + 60
            while 'Python syntax error' not in log_path.read_text():
                assert time.monotonic() < deadline, log_path.read_text()
                time.sleep(.2)
            assert call() == 'Welcome, Cloudflare Python!'
            source.write_text(code)
            wait_for('Hello, Cloudflare Python!')
            before = log_path.read_text().count('change detected')
            for _ in range(5):
                source.read_text()
                assert call() == 'Hello, Cloudflare Python!'
            time.sleep(1)
            assert log_path.read_text().count('change detected') == before
            (root / 'pyproject.toml').write_text('[project]\nname="python-hello"\ndependencies=["numpy>=2", "pydantic>=2.12,<3", "absent; sys_platform == \'linux\'"]\n')
            source.write_text('''from workers import Response, WorkerEntrypoint
from pydantic import BaseModel
import numpy as np
class Input(BaseModel):
    values: list[float]
class Default(WorkerEntrypoint):
    async def fetch(self, request):
        data = Input.model_validate(await request.json())
        return Response.from_json({"mean": float(np.mean(data.values)), "greeting": self.env.GREETING})
''')
            wait_for({'mean':3.0,'greeting':'Cloudflare Python'}, {'values':[1,2,6]})
            assert len(shared_objects()) > len(first_artifacts), 'declared wheels must be separate shared modules'
            assert not marker.exists(), 'celld invoked an external language tool'
            print('Built-in Cloudflare Python, env, JSON, Pydantic, NumPy, reload, invalid-edit recovery, shared runtime/wheel reuse, no external CLI: PASS')
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            print(log_path.read_text()[-12000:])
