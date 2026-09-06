"""Matched native Python/TypeScript baseline and candidate on one runner."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import statistics
import subprocess
import time
import urllib.request
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]

def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0))
        return sock.getsockname()[1]

def stop(process):
    process.terminate()
    try: process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill(); process.wait()

def http(url, data=None):
    req = urllib.request.Request(url, data=json.dumps(data).encode() if data is not None else None,
        headers={'content-type':'application/json'})
    with urllib.request.urlopen(req, timeout=60) as response:
        body = response.read()
        return json.loads(body) if body else None

@contextmanager
def node(binary, project, log_path, env, isolates):
    port = free_port()
    env = dict(env, CELLD_INTERNAL_DEV_STORE=str(project / '.celld/dev/objects.sqlite3'),
        CELLD_WATCH=str(project / '.celld/dev/runtime'), CELLD_NODE='bench-'+uuid.uuid4().hex[:12],
        CELLD_MAX_STATELESS_ISOLATES=str(isolates), RUST_LOG='info')
    with log_path.open('w') as log:
        start = time.perf_counter()
        process = subprocess.Popen([str(binary),'--no-control-plane','--bucket','celld-dev',
            '--listen',f'127.0.0.1:{port}','--internal-listen','127.0.0.1:0'], env=env, stdout=log, stderr=log)
        try:
            deadline = time.monotonic()+60
            while time.monotonic()<deadline:
                content = log_path.read_text()
                internal = re.search(r'celld internal listening on (127\.0\.0\.1:\d+)',content)
                if process.poll() is not None: raise RuntimeError(content[-6000:])
                if internal:
                    try:
                        ready = http(f'http://127.0.0.1:{port}/.well-known/celld/health')['ok']
                    except OSError: ready = False
                    if ready:
                        yield port, internal[1], (time.perf_counter()-start)*1000
                        return
                time.sleep(.01)
            raise RuntimeError('readiness timeout: '+log_path.read_text()[-6000:])
        finally: stop(process)

def publish(binary, project, log_path, env):
    start=time.perf_counter()
    with log_path.open('w') as log:
        process=subprocess.Popen([str(binary),'dev',str(project),'--port',str(free_port()),'--logs'],
            env=env,stdout=log,stderr=log)
        try:
            deadline=time.monotonic()+90
            while time.monotonic()<deadline:
                content=log_path.read_text()
                if 'ready ' in content: return (time.perf_counter()-start)*1000
                if process.poll() is not None: raise RuntimeError(content[-6000:])
                time.sleep(.05)
            raise RuntimeError(content[-6000:])
        finally: stop(process)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--baseline-runtime',type=Path,required=True)
    parser.add_argument('--candidate-runtime',type=Path)
    parser.add_argument('--variants',default='baseline,typescript')
    parser.add_argument('--rounds',type=int,default=1)
    parser.add_argument('--seconds',type=float,default=5)
    parser.add_argument('--clients',default='32,256')
    parser.add_argument('--isolates',default='1,4')
    parser.add_argument('--delays',default='10')
    args=parser.parse_args()
    out=HERE/'build'/uuid.uuid4().hex[:12];out.mkdir(parents=True)
    binary=args.binary.resolve()
    driver=out/'load'
    subprocess.run(['go','build','-o',str(driver),'.'],cwd=HERE/'load',check=True)
    address=f'127.0.0.1:{free_port()}'
    upstream=subprocess.Popen([str(driver),'--serve',address],stdout=subprocess.DEVNULL)
    report=dict(source_sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
        binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), native_source=os.environ.get('CELLD_NATIVE_SHA'),
        cpu=subprocess.check_output(['lscpu'],text=True),
        run_url=f"https://github.com/{os.environ.get('GITHUB_REPOSITORY')}/actions/runs/{os.environ.get('GITHUB_RUN_ID')}",
        store='local SQLite development object store; cached artifacts, no remote S3',
        protocol='HTTP/1.1 keep-alive; unique IDs, no retries, every response validated',samples=[])
    def record(row):
        report['samples'].append(row)
        (out/'measurements.json').write_text(json.dumps(report,indent=2)+'\n')
        print('NATIVE_SAMPLE='+json.dumps(row),flush=True)
    try:
        for _ in range(100):
            try: http(f'http://{address}/stats');break
            except OSError: time.sleep(.05)
        else: raise RuntimeError('upstream did not start')
        for workload, delay in [('hello',0)]+[('io',int(d)) for d in args.delays.split(',')]:
            projects={}
            for variant in args.variants.split(','):
                project=out/f'{variant}-{workload}-{delay}'; project.mkdir()
                python=variant!='typescript'
                entry='worker.py' if python else 'worker.ts'
                shutil.copyfile(HERE/entry,project/entry)
                config={'name':'native-benchmark','main':entry,'compatibility_date':'2026-09-05',
                    'vars':{'UPSTREAM':f'http://{address}/price?delay_ms={delay}' if workload=='io' else ''}}
                if variant == 'monty':
                    config['python_runtime'] = 'monty'
                    if workload == 'hello':
                        (project/entry).write_text("def hello(name: str):\n    if not 1 <= len(name) <= 128: raise ValueError('invalid name')\n    return 'Hello, ' + name\n")
                    else:
                        shutil.copyfile(HERE/'monty.py',project/entry)
                elif python: config['compatibility_flags']=['python_workers']
                (project/'wrangler.json').write_text(json.dumps(config))
                env={key:value for key,value in os.environ.items() if not key.startswith('CELLD_')}
                runtime=args.candidate_runtime if variant=='candidate' else args.baseline_runtime
                env.update(CELLD_V8_HEAP_LIMIT_MB='256',CELLD_MODULE_CACHE=str(out/'module-cache'))
                if python and variant != 'monty': env['CELLD_PYTHON_RUNTIME']=str(runtime.resolve())
                published=publish(binary,project,out/(project.name+'-publish.log'),env)
                projects[variant]=(project,env,published)
            for isolates in map(int,args.isolates.split(',')):
                for clients in map(int,args.clients.split(',')):
                    for round_ in range(args.rounds):
                        variants=list(projects)
                        if round_%2: variants.reverse()
                        for variant in variants:
                            project,env,published=projects[variant]
                            log=out/f'{variant}-{workload}-{delay}-{isolates}-{clients}-{round_}.log'
                            with node(binary,project,log,env,isolates) as (port,internal,ready):
                                start=time.perf_counter()
                                value=http(f'http://127.0.0.1:{port}/hello',{'name':'first'})
                                expected={'result':'Hello, first'} if workload=='hello' else {'result':{'customer':'first','total_cents':3998,'currency':'USD','trace':'first'}}
                                assert value==expected,value
                                first=(time.perf_counter()-start)*1000
                                command=[str(driver),'--url',f'http://127.0.0.1:{port}/hello','--workload',workload,'--clients',str(clients)]
                                subprocess.run(command+['--count','4'],check=True,capture_output=True)
                                http(f'http://{address}/reset')
                                result=subprocess.run(command+['--seconds',str(args.seconds)],capture_output=True,text=True)
                                if result.returncode: raise RuntimeError(result.stdout+result.stderr)
                                row=json.loads(result.stdout)
                                assert row['errors']==0 and row['requests']>0
                                stats=http(f'http://{address}/stats')
                                if workload=='io': assert stats['requests']==stats['completed']==row['requests'] and stats['active']==0,(row,stats)
                                state=http(f'http://{internal}/state')
                                row.update(variant=variant,workload=workload,delay_ms=delay,isolates=isolates,round=round_,
                                    deploy_ready_ms=published,native_ready_ms=ready,first_request_ms=first,
                                    rss_bytes=state['rss_bytes'],in_use_bytes=state['in_use_bytes'],upstream=stats, runtime_state=state)
                                if workload == 'hello' and isolates == 1 and clients == int(args.clients.split(',')[0]) and round_ == 0:
                                    idle_start = time.perf_counter()
                                    deadline = time.monotonic()+40
                                    while time.monotonic()<deadline:
                                        content=log.read_text()
                                        if content.count('isolate started') == content.count('isolate freed') and 'isolate freed' in content:
                                            break
                                        time.sleep(.1)
                                    else: raise AssertionError('idle worker did not scale to zero: '+log.read_text()[-3000:])
                                    row['idle_scale_to_zero_ms']=(time.perf_counter()-idle_start)*1000
                                    before=log.read_text().count('isolate started')
                                    start=time.perf_counter()
                                    assert http(f'http://127.0.0.1:{port}/hello',{'name':'wake'}) == {'result':'Hello, wake'}
                                    row['wake_request_ms']=(time.perf_counter()-start)*1000
                                    assert log.read_text().count('isolate started') > before, 'wake did not create a new isolate'
                                record(row)
        report['summary']=[]
        groups={(r['variant'],r['workload'],r['delay_ms'],r['isolates'],r['clients']) for r in report['samples']}
        for variant,workload,delay,isolates,clients in sorted(groups):
            rows=[r for r in report['samples'] if (r['variant'],r['workload'],r['delay_ms'],r['isolates'],r['clients'])==(variant,workload,delay,isolates,clients)]
            summary=dict(variant=variant,workload=workload,delay_ms=delay,isolates=isolates,clients=clients,n=len(rows))
            for key in ['rps','p50_ms','p95_ms','p99_ms','rss_bytes','first_request_ms','native_ready_ms']:
                summary[key]=statistics.median(r[key] for r in rows)
            report['summary'].append(summary)
        (out/'measurements.json').write_text(json.dumps(report,indent=2)+'\n')
        print('NATIVE_REPORT='+json.dumps(report),flush=True)
    finally: stop(upstream)

if __name__=='__main__':main()
