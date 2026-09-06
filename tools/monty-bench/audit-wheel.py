"""Inspect a checksum-pinned real Pyodide extension without executing it."""
import hashlib
import json
from pathlib import Path
import subprocess
import urllib.request
import zipfile

OUT=Path(__file__).resolve().parent/'build/wasm-audit'
OUT.mkdir(parents=True,exist_ok=True)
NAME='pydantic_core-2.41.5-cp314-cp314-pyemscripten_2026_0_wasm32.whl'
SHA='9802bd7a5e6679ec4c13be778f19f021e6147459c89cc916b913f7970f86dddb'
URL='https://cdn.jsdelivr.net/pyodide/v314.0.6/full/'+NAME
wheel=OUT/NAME
if not wheel.exists():
    wheel.write_bytes(urllib.request.urlopen(URL,timeout=60).read())
assert hashlib.sha256(wheel.read_bytes()).hexdigest()==SHA
with zipfile.ZipFile(wheel) as archive:
    names=[n for n in archive.namelist() if n.endswith('.so')]
    assert len(names)==1,names
    wasm=archive.read(names[0])
    assert wasm[:4]==b'\x00asm'
    (OUT/'pydantic-core.wasm').write_bytes(wasm)
script='''
const fs=require('node:fs');
const module=new WebAssembly.Module(fs.readFileSync(process.argv[1]));
const imports=WebAssembly.Module.imports(module);
const exports=WebAssembly.Module.exports(module);
console.log(JSON.stringify({imports,exports,cpython_imports:imports.filter(i=>/^_?Py/.test(i.name))}));
'''
result=json.loads(subprocess.check_output(['node','-e',script,str(OUT/'pydantic-core.wasm')],text=True))
assert any(i['name']=='PyList_New' for i in result['cpython_imports'])
assert any(i['name']=='PyInit__pydantic_core' for i in result['exports'])
result.update(wheel=NAME,wheel_sha256=SHA,url=URL,extension=names[0],wasm_bytes=len(wasm))
(OUT/'audit.json').write_text(json.dumps(result,indent=2)+'\n')
print('WASM_AUDIT='+json.dumps(result))
