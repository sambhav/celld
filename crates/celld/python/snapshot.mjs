// Build-time only. Snapshot the interpreter and portable stdlib imports, never
// application code, user data, third-party DSOs, or deployment credentials.
import {writeFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {gzipSync} from 'node:zlib';
const [runtime, output] = process.argv.slice(2);
const {loadPyodide} = await import(pathToFileURL(`${runtime}/pyodide.mjs`));
const py = await loadPyodide({_makeSnapshot:true});
py.runPython(`
import asyncio, base64, contextvars, dataclasses, inspect, json, re, typing
from pyodide.http import pyfetch
`);
const snapshot = py.makeMemorySnapshot();
// Catch an incompatible upstream format before producing a deployment artifact.
const restored = await loadPyodide({_loadSnapshot:snapshot});
if (restored.runPython('json.dumps({"snapshot": True})') !== '{"snapshot": true}')
  throw new Error('Python snapshot verification failed');
await writeFile(output, gzipSync(snapshot, {level:9}));
console.log(JSON.stringify({bytes:snapshot.byteLength}));
