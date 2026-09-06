"""Diagnostic CPython call profile. Its instrumented throughput is not a benchmark."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import uuid

from measure import HERE, free_port, http, node, publish, stop

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--runtime', type=Path, required=True)
args = parser.parse_args()
out = HERE / 'build' / ('profile-' + uuid.uuid4().hex[:8])
out.mkdir(parents=True)
driver = out / 'load'
subprocess.run(['go','build','-o',str(driver),'.'], cwd=HERE/'load', check=True)
address = f'127.0.0.1:{free_port()}'
upstream = subprocess.Popen([str(driver),'--serve',address], stdout=subprocess.DEVNULL)
reports = []
try:
    for _ in range(100):
        try: http(f'http://{address}/stats'); break
        except OSError: time.sleep(.05)
    else: raise RuntimeError('upstream did not start')
    for workload in ['hello','io']:
        project = out / workload
        project.mkdir()
        shutil.copyfile(HERE/'worker.py', project/'app.py')
        (project/'worker.py').write_text('''import cProfile, pstats
from app import Default as Application
from workers import Response
profile = cProfile.Profile()
class Default(Application):
    async def fetch(self, request):
        if request.method == 'GET':
            profile.disable()
            stats = pstats.Stats(profile)
            rows = []
            for (file, line, name), (primitive, calls, own, total, callers) in stats.stats.items():
                rows.append(dict(file=file, line=line, name=name, calls=calls, self_seconds=own, total_seconds=total))
            profile.clear()
            return Response.from_json(sorted(rows, key=lambda row: row['self_seconds'], reverse=True)[:50])
        profile.enable()
        return await super().fetch(request)
''')
        config = dict(name='python-profile', main='worker.py', compatibility_date='2026-09-05',
            compatibility_flags=['python_workers'], vars=dict(UPSTREAM=f'http://{address}/price?delay_ms=10' if workload=='io' else ''))
        (project/'wrangler.json').write_text(json.dumps(config))
        env = {k:v for k,v in os.environ.items() if not k.startswith('CELLD_')}
        env.update(CELLD_PYTHON_RUNTIME=str(args.runtime.resolve()), CELLD_MODULE_CACHE=str(out/'module-cache'), CELLD_V8_HEAP_LIMIT_MB='256')
        publish(args.binary.resolve(), project, out/(workload+'-publish.log'), env)
        with node(args.binary.resolve(), project, out/(workload+'.log'), env, 1) as (port, _, _ready):
            url = f'http://127.0.0.1:{port}/hello'
            command = [str(driver),'--url',url,'--workload',workload,'--clients','32']
            subprocess.run(command+['--count','4'], check=True, capture_output=True)
            http(url)  # Clear warmup profile.
            http(f'http://{address}/reset')
            result = subprocess.run(command+['--seconds','5'], check=True, capture_output=True, text=True)
            measured = json.loads(result.stdout)
            stats = http(f'http://{address}/stats')
            if workload == 'io': assert stats['requests'] == stats['completed'] == measured['requests'] and stats['active'] == 0
            rows = http(url)
            reports.append(dict(workload=workload, requests=measured['requests'], rows=rows))
            print('PYTHON_PROFILE='+json.dumps(reports[-1]), flush=True)
    (out/'profiles.json').write_text(json.dumps(reports, indent=2)+'\n')
finally:
    stop(upstream)
