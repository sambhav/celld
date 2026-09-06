// Python and Node are source-build tools only. Official binaries embed all
// runtime assets and never launch them (or pycelld) for Python workers.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=python");
    println!("cargo:rerun-if-changed=../../tools/python-runtime");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("python-runtime");
    let status = Command::new("python3")
        .arg(manifest.join("../../tools/python-runtime/build.py"))
        .arg(&output)
        .status()
        .expect("Building celld from source requires Python 3.11+ and Node/npm for embedded Python assets");
    assert!(status.success(), "prepare embedded Python runtime assets");
    println!("cargo:rustc-env=CELLD_PYTHON_RUNTIME={}", output.display());
}
