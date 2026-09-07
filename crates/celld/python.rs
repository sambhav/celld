//! Compile a Python entrypoint into a native Monty artifact.
use anyhow::{bail, Context};
use celld_monty::exports::{durable_classes, Module};
use std::path::Path;

/// A versioned source artifact; never evaluated by V8.
pub(crate) const MAGIC: &str = "# celld:monty-native-v1\n";

pub(crate) fn is_python(entry: &str) -> bool {
    Path::new(entry).extension().is_some_and(|ext| ext == "py")
}

pub(crate) fn bundle(root: &Path, entry: &str) -> anyhow::Result<Vec<u8>> {
    let source = std::fs::read_to_string(root.join(entry)).context("read Monty source")?;
    if source.len() > 256 * 1024 {
        bail!("Monty source exceeds 256 KiB");
    }
    Module::compile(&source).map_err(anyhow::Error::msg)?;
    for class in durable_classes(&source).map_err(anyhow::Error::msg)? {
        Module::compile_class(&source, &class).map_err(anyhow::Error::msg)?;
    }
    Ok(format!("{MAGIC}{source}").into_bytes())
}
