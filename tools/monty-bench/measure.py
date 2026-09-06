"""Same-runner feasibility comparison; Monty/Rust servers are NOT celld backends."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time
import urllib.request
import uuid

HERE = Path(__file__).resolve().parent
NATIVE = HERE.parent / 'python-bench'
sys.path.insert(0, str(NATIVE))
from measure import free_port, http, node, publish, stop


@contextmanager
def prototype(binary, variant, threads, upstream, log_path):
    port = free_port()
    with log_path.open('w') as log:
        start = time.perf_counter()
        process = subprocess.Popen([str(binary), str(port), str(threads), upstream, variant], stdout=log, stderr=log)
        try:
            for _ in range(600):
                if process.poll() is not None:
                    raise RuntimeError(log_path.read_text())
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/health', timeout=1) as response:
                        assert response.read() == b'ok'
                    yield port, process.pid, (time.perf_counter()-start)*1000
                    return
                except OSError:
                    time.sleep(.01)
            raise RuntimeError('prototype readiness timeout')
        finally:
            stop(process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--celld', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--monty', type=Path, required=True)
    parser.add_argument('--rounds', type=int, default=3)
    parser.add_argument('--seconds', type=float, default=5)
    args = parser.parse_args()
    out = HERE/'build'/uuid.uuid4().hex[:12]
    out.mkdir(parents=True)
    celld, monty = args.celld.resolve(), args.monty.resolve()
    driver = out/'load'
    subprocess.run(['go', 'build', '-o', str(driver), '.'], cwd=NATIVE/'load', check=True)
    address = f'127.0.0.1:{free_port()}'
    upstream = subprocess.Popen([str(driver), '--serve', address], stdout=subprocess.DEVNULL)
    report = dict(source_sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
        monty_revision='af272c3116e2525249103f960b79086fd250bcef',
        celld_sha256=hashlib.sha256(celld.read_bytes()).hexdigest(), monty_sha256=hashlib.sha256(monty.read_bytes()).hexdigest(),
        run_url=f"https://github.com/{os.environ.get('GITHUB_REPOSITORY')}/actions/runs/{os.environ.get('GITHUB_RUN_ID')}",
        cpu=subprocess.check_output(['lscpu'],text=True),
        scope='Monty and Rust are standalone axum prototypes; Python and TypeScript are celld workers. No Monty celld integration or crash isolation is measured.',
        slots='Monty/Rust: Tokio execution threads; Python/TypeScript: celld max stateless isolates. These are different concurrency mechanisms.',
        store='celld uses local SQLite development store and warm artifact cache; no remote S3', samples=[])
    try:
        for _ in range(100):
            try: http(f'http://{address}/stats'); break
            except OSError: time.sleep(.05)
        else: raise RuntimeError('upstream failed')
        for workload, delay in [('hello',0), ('io',10), ('io',50)]:
            upstream_url = f'http://{address}/price?delay_ms={delay}' if delay else ''
            projects = {}
            for variant in ['python','typescript']:
                project = out/f'{variant}-{workload}-{delay}'; project.mkdir()
                entry = 'worker.py' if variant=='python' else 'worker.ts'
                shutil.copyfile(NATIVE/entry, project/entry)
                config = dict(name='native-benchmark', main=entry, compatibility_date='2026-09-05', vars={'UPSTREAM':upstream_url})
                if variant=='python': config['compatibility_flags']=['python_workers']
                (project/'wrangler.json').write_text(json.dumps(config))
                env = {k:v for k,v in os.environ.items() if not k.startswith('CELLD_')}
                env.update(CELLD_V8_HEAP_LIMIT_MB='256', CELLD_MODULE_CACHE=str(out/'cache'), CELLD_PYTHON_RUNTIME=str(args.runtime.resolve()))
                publish(celld, project, out/f'{project.name}-publish.log', env)
                projects[variant] = project, env
            for slots in [1,4]:
                for round_ in range(args.rounds):
                    variants = ['python','typescript','monty','rust']
                    # Rotate order across rounds; no simultaneous competing servers.
                    variants = variants[round_:]+variants[:round_]
                    for variant in variants:
                        log = out/f'{variant}-{workload}-{delay}-{slots}-{round_}.log'
                        if variant in projects:
                            project, env = projects[variant]
                            server = node(celld, project, log, env, slots)
                        else:
                            server = prototype(monty, variant, slots, upstream_url, log)
                        with server as (port, info, ready):
                            endpoint = f'http://127.0.0.1:{port}/hello'
                            start = time.perf_counter()
                            value = http(endpoint, {'name':'first'})
                            expected = {'result':'Hello, first'} if not delay else {'result':{'customer':'first','total_cents':3998,'currency':'USD','trace':'first'}}
                            assert value == expected, value
                            first = (time.perf_counter()-start)*1000
                            # Invalid input must be rejected before any upstream call.
                            try: http(endpoint, {'name':''})
                            except urllib.error.HTTPError as e: assert e.code == 422, e.code
                            else: raise AssertionError('invalid input accepted')
                            command = [str(driver),'--url',endpoint,'--workload',workload,'--clients','256']
                            subprocess.run(command+['--count','4'],check=True,capture_output=True)
                            http(f'http://{address}/reset')
                            result = subprocess.run(command+['--seconds',str(args.seconds)],capture_output=True,text=True,timeout=90)
                            if result.returncode: raise RuntimeError(result.stdout+result.stderr)
                            row = json.loads(result.stdout)
                            assert row['errors']==0 and row['requests']>0, row
                            stats = http(f'http://{address}/stats')
                            if delay: assert stats['requests']==stats['completed']==row['requests'] and stats['active']==0, (stats,row)
                            if variant in projects: rss = http(f'http://{info}/state')['rss_bytes']
                            else: rss = int(Path(f'/proc/{info}/statm').read_text().split()[1])*os.sysconf('SC_PAGE_SIZE')
                            row.update(variant=variant,workload=workload,delay_ms=delay,slots=slots,round=round_,ready_ms=ready,first_request_ms=first,rss_bytes=rss,upstream=stats)
                            report['samples'].append(row)
                            (out/'measurements.json').write_text(json.dumps(report,indent=2)+'\n')
                            print('MONTY_SAMPLE='+json.dumps(row),flush=True)
        report['summary'] = []
        for key in sorted({(r['variant'],r['workload'],r['delay_ms'],r['slots']) for r in report['samples']}):
            rows = [r for r in report['samples'] if (r['variant'],r['workload'],r['delay_ms'],r['slots'])==key]
            row = dict(zip(['variant','workload','delay_ms','slots'],key))
            for field in ['rps','p50_ms','p95_ms','p99_ms','ready_ms','first_request_ms','rss_bytes']:
                if field in rows[0]: row[field]=statistics.median(r[field] for r in rows)
            report['summary'].append(row)
        (out/'measurements.json').write_text(json.dumps(report,indent=2)+'\n')
        print('MONTY_REPORT='+json.dumps(report),flush=True)
    finally:
        stop(upstream)


if __name__=='__main__': main()
