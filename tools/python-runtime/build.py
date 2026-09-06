"""Prepare binary-embedded Python assets during celld's source build only.

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
    (stage / 'pyodide.asm.mjs').write_text('import {WebAssembly} from "./wasm.js";\nimport {assetFetch as fetch} from "./assets.js";\nconst location="https://celld-python.invalid/runtime/";\n' + source)
    shutil.copyfile(runtime / 'pyodide.asm.wasm', stage / 'pyodide.asm.wasm')
    for name in ('host.js', 'wasm.js', 'assets.js'):
        shutil.copyfile(SOURCE / name, stage / name)
    snapshot = cache / 'baseline.snapshot.gz'
    snapshot_key = hashlib.sha256((SOURCE / 'snapshot.mjs').read_bytes() + json.dumps(LOCK,sort_keys=True).encode()).hexdigest()
    key_file = snapshot.with_suffix('.key')
    if not snapshot.exists() or not key_file.exists() or key_file.read_text() != snapshot_key:
        subprocess.run(['node', str(SOURCE / 'snapshot.mjs'), str(runtime), str(snapshot)], check=True)
        key_file.write_text(snapshot_key)
    assets = {'/runtime/python_stdlib.zip':base64.b64encode((runtime / 'python_stdlib.zip').read_bytes()).decode(),
        '/runtime/baseline.snapshot.gz':base64.b64encode(snapshot.read_bytes()).decode()}
    # The Rust builder appends the declared, checksum-verified wheel assets.
    (stage / 'asset-data.js').write_text('export const assets={...'+json.dumps(assets)+',...JSON.parse("__CELLD_PYTHON_PACKAGES__")};\n')
    workers = {p.relative_to(SOURCE).as_posix():p.read_text() for p in sorted((SOURCE / 'workers').glob('*.py'))}
    (stage / 'python-sources.js').write_text('export const workers='+json.dumps(workers)+';\nexport const dispatch='+json.dumps((SOURCE / 'dispatch.py').read_text())+';\n')
    subprocess.run([str(node / 'node_modules/.bin/esbuild'),str(stage / 'host.js'),'--bundle','--format=esm','--platform=browser',
        '--target=es2024','--external:node:*','--external:cloudflare:*','--loader:.wasm=copy','--asset-names=[name]',
        '--outfile='+str(stage / 'out/bundle.js')], check=True)
    for source, destination in [(stage / 'out/bundle.js', 'runtime.js.gz'), (stage / 'pyodide.asm.wasm', 'core.wasm.gz'),
                                (runtime / 'pyodide-lock.json', 'catalog.json.gz')]:
        content = source.read_bytes()
        if destination == 'runtime.js.gz':
            notices = '\n'.join(p.read_text() for p in sorted((SOURCE / 'licenses').glob('*.txt')))
            content = ('/*!\n' + notices.replace('*/', '* /') + '\n*/\n').encode() + content
        (OUT / destination).write_bytes(gzip.compress(content, mtime=0))
