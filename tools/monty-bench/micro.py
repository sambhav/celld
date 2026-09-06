"""Interpreter-only comparison; same Python fixture, no HTTP and no host calls."""
import json
import time
from pathlib import Path

scope = {}
exec(Path(__file__).with_name('worker.py').read_text(), scope)
handle = scope['handle']
raw = '{"name":"benchmark"}'
assert json.loads(handle(raw, False)) == {'result':'Hello, benchmark'}
for _ in range(1000): handle(raw, False)
samples = []
for _ in range(5):
    start = time.perf_counter()
    for _ in range(10000): handle(raw, False)
    samples.append((time.perf_counter()-start)*1e9/10000)
print('PYTHON_MICRO='+json.dumps({'ns_per_call':samples}))
