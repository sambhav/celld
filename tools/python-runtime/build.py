"""Build a separately distributed, versioned Python runtime artifact.

Never runs during celld dev/deploy/serve. Uses no pycelld installation.
Runtime bytes are pinned by SHA-256, the esbuild version is pinned as well.
"""
import base64
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'crates/celld/python'
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else SOURCE / 'generated'
LOCK = json.loads((Path(__file__).parent / 'runtime-lock.json').read_text())


def replace(source, old, new):
    if source.count(old) != 1:
        raise ValueError('Pinned Pyodide port no longer matches: ' + old[:70])
    return source.replace(old, new)


OUT.mkdir(parents=True, exist_ok=True)
cache = ROOT / 'target/python-runtime-cache'
cache.mkdir(parents=True, exist_ok=True)
runtime = cache / LOCK['pyodide']
runtime.mkdir(exist_ok=True)
if not all((runtime / name).exists() for name in LOCK['runtime_files']):
    result = subprocess.run(['npm','pack',f"pyodide@{LOCK['pyodide']}",'--json','--pack-destination',str(runtime)],
        capture_output=True, text=True, check=True)
    archive = runtime / json.loads(result.stdout)[0]['filename']
    with tarfile.open(archive) as tar:
        for name in LOCK['runtime_files']:
            (runtime / name).write_bytes(tar.extractfile('package/' + name).read())
for name, expected in LOCK['runtime_files'].items():
    if hashlib.sha256((runtime / name).read_bytes()).hexdigest() != expected:
        raise ValueError('Runtime checksum mismatch: ' + name)
node = cache / 'node'
if not (node / 'node_modules/.bin/esbuild').exists():
    subprocess.run(['npm','install','--prefix',str(node),'--ignore-scripts','--no-audit','--no-fund','esbuild@0.25.12'], check=True)
with tempfile.TemporaryDirectory(dir=cache) as temporary:
    stage = Path(temporary)
    source = (runtime / 'pyodide.mjs').read_text()
    # Release bootstrap input from API.config after the loader takes its local copy.
    source = replace(source, 'return t.noInitialRun=!0,t.INITIAL_MEMORY=i.length,i',
        'return delete e._loadSnapshot,t.noInitialRun=!0,t.INITIAL_MEMORY=i.length,i')
    source = replace(source, 'import("ws")', 'Promise.reject(new Error("Node ws unavailable in celld"))')
    source = replace(source, '_(e+"pyodide.asm.wasm")', '{response:true}')
    source = replace(source, 'WebAssembly.instantiateStreaming(n,s)', 'Promise.resolve({instance:new WebAssembly.Instance(_pythonWasm,s),module:_pythonWasm})')
    source = ('import _pythonWasm from "./pyodide.asm.wasm";\nimport {WebAssembly} from "./wasm.js";\n'
        'import {assetFetch as fetch} from "./assets.js";\nconst process=undefined; const location="https://celld-python.invalid/runtime/";\n' + source)
    (stage / 'pyodide.mjs').write_text(source)
    source = (runtime / 'pyodide.asm.mjs').read_text()
    source = replace(source, 'import("ws")', 'Promise.reject(new Error("Node ws unavailable in celld"))')
    source = replace(source, 'var ENVIRONMENT_IS_NODE=globalThis.process?.versions?.node&&globalThis.process?.type!="renderer";', 'var ENVIRONMENT_IS_NODE=false;')
    source = replace(source, 'var ENVIRONMENT_IS_WORKER=!!globalThis.WorkerGlobalScope;', 'var ENVIRONMENT_IS_WORKER=true;')
    source = replace(source, 'typeof globalThis.MessageChannel=="function"', 'false')
    source = replace(source, 'function me(t,e=0){e<=2?bo(t):setTimeout(t,e)}',
        'function me(t,e=0){schedulePythonCallback(t,e)}')
    (stage / 'pyodide.asm.mjs').write_text('import {schedulePythonCallback} from "./scheduling-context.mjs";\nimport {WebAssembly} from "./wasm.js";\nimport {assetFetch as fetch} from "./assets.js";\nconst location="https://celld-python.invalid/runtime/";\n' + source)
    shutil.copyfile(runtime / 'pyodide.asm.wasm', stage / 'pyodide.asm.wasm')
    for name in ('host.js', 'invoke.mjs', 'schedule.mjs', 'scheduling-context.mjs', 'wasm.js', 'assets.js'):
        shutil.copyfile(SOURCE / name, stage / name)
    snapshot = cache / 'baseline.snapshot.gz'
    snapshot_key = hashlib.sha256((SOURCE / 'snapshot.mjs').read_bytes() + json.dumps(LOCK,sort_keys=True).encode()).hexdigest()
    key_file = snapshot.with_suffix('.key')
    expected_snapshot = json.loads(key_file.read_text()) if key_file.exists() and key_file.read_text().startswith('{') else {}
    if not snapshot.exists() or expected_snapshot.get('source') != snapshot_key or expected_snapshot.get('sha256') != hashlib.sha256(snapshot.read_bytes()).hexdigest():
        subprocess.run(['node', str(SOURCE / 'snapshot.mjs'), str(runtime), str(snapshot)], check=True)
        key_file.write_text(json.dumps({'source':snapshot_key,'sha256':hashlib.sha256(snapshot.read_bytes()).hexdigest()}))
    # Data stays in separate text modules, shared by content hash across apps.
    (stage / 'asset-data.js').write_text("import stdlib from './python-stdlib.b64';\nimport snapshot from './python-snapshot.b64';\nexport const assets={'/runtime/python_stdlib.zip':stdlib, '/runtime/baseline.snapshot.gz':snapshot};\n")
    workers = {p.relative_to(SOURCE).as_posix():p.read_text() for p in sorted((SOURCE / 'workers').glob('*.py'))}
    (stage / 'python-sources.js').write_text('export const workers='+json.dumps(workers)+';\nexport const dispatch='+json.dumps((SOURCE / 'dispatch.py').read_text())+';\n')
    subprocess.run([str(node / 'node_modules/.bin/esbuild'),str(stage / 'host.js'),'--bundle','--format=esm','--platform=browser',
        '--target=es2024','--external:*.b64','--external:node:*','--external:cloudflare:*','--loader:.wasm=copy','--asset-names=[name]',
        '--outfile='+str(stage / 'out/bundle.js')], check=True)
    notices = '\n'.join(p.read_text() for p in sorted((SOURCE / 'licenses').glob('*.txt')))
    files = {
        '_python_runtime.js': ('esmodule', ('/*!\n' + notices.replace('*/', '* /') + '\n*/\n').encode() + (stage / 'out/bundle.js').read_bytes()),
        'pyodide.asm.wasm': ('wasm', (stage / 'pyodide.asm.wasm').read_bytes()),
        'python-stdlib.b64': (None, base64.b64encode((runtime / 'python_stdlib.zip').read_bytes())),
        'python-snapshot.b64': (None, base64.b64encode(snapshot.read_bytes())),
        'catalog.json': (None, (runtime / 'pyodide-lock.json').read_bytes()),
    }
    manifest = {'schema_version': 1, 'abi': 'celld-python-v1', 'pyodide': LOCK['pyodide'], 'workers_sdk': LOCK['workers_sdk'], 'files': {}}
    for name, (kind, data) in files.items():
        (OUT / name).write_bytes(data)
        manifest['files'][name] = {'bytes':len(data), 'sha256':hashlib.sha256(data).hexdigest(), 'kind':kind}
    manifest_bytes = (json.dumps(manifest, sort_keys=True, indent=2) + '\n').encode()
    (OUT / 'runtime.json').write_bytes(manifest_bytes)
    (OUT / 'runtime.sha256').write_text(hashlib.sha256(manifest_bytes).hexdigest() + '\n')
    print('Python runtime artifact: ' + str(OUT) + ' (' + hashlib.sha256(manifest_bytes).hexdigest() + ')')
