# Python Workers

Install the companion [celld Python SDK](https://github.com/sambhav/celld-python)
branch `feat/python-workers` and esbuild 0.25.12 on the build/development machine.

```sh
python -m pip install 'celld @ git+https://github.com/sambhav/celld-python@feat/python-workers'
npm install -g esbuild@0.25.12
pycelld lock examples/python
celld dev examples/python
pycelld call hello name=Sam
```

`celld dev` watches the original Python project. Valid edits are built and
published; invalid edits keep the previous deployment. `celld deploy
examples/python` uses the same compiler and the normal celld deployment flags.
Set `CELLD_PYCELLD` to an explicit builder executable if needed (an executable
path, never a shell command).

The builder emits a self-contained JS/WASM deployment with the pinned CPython
runtime, compressed interpreter snapshot, locked packages, and generated client.
Only celld and S3 are required on server nodes. Optional dependencies belong in
`pyproject.toml`; use Pyodide-compatible wheels for native Python extensions.

This accepts Wrangler JSON/JSONC Python entrypoints. The app API is the celld
function API; Cloudflare `WorkerEntrypoint`, arbitrary Cloudflare bindings and
unmodified Wrangler's Python upload format are not supported by this adapter.
