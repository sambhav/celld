//! Build Python Wrangler entrypoints locally; deployment stays self-contained.
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn is_python(entry: Option<&str>) -> bool {
    entry.is_some_and(|entry| Path::new(entry).extension().is_some_and(|ext| ext == "py"))
}

fn builder_command(binary: &std::ffi::OsStr, config: &Path, output: &Path) -> Command {
    let mut command = Command::new(binary);
    command.arg("build").arg(config).arg("--out").arg(output);
    command
}

pub(crate) fn prepare(config: &Path) -> anyhow::Result<PathBuf> {
    let config = config
        .canonicalize()
        .context("resolve Python Wrangler config")?;
    let output = config
        .parent()
        .context("Python config has no parent")?
        .join(".celld-python")
        .join("build");
    let binary = std::env::var_os("CELLD_PYCELLD").unwrap_or_else(|| "pycelld".into());
    let result = builder_command(&binary, &config, &output)
        .output()
        .context("run Python builder; install celld-python (CLI: pycelld), then run `pycelld lock` in the project")?;
    if !result.status.success() {
        bail!(
            "Python build failed ({}):\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let built_config = output.join("wrangler.json");
    if !built_config.is_file() {
        bail!("Python builder did not produce {}", built_config.display());
    }
    Ok(built_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_python_entrypoints_select_the_builder() {
        assert!(is_python(Some("src/worker.py")));
        for entry in [
            None,
            Some("index.ts"),
            Some("worker.py.js"),
            Some("worker.PY"),
        ] {
            assert!(!is_python(entry));
        }
    }

    #[test]
    fn paths_are_arguments_not_shell_code() {
        let config = Path::new("/tmp/project with spaces/$(touch bad)/wrangler.jsonc");
        let output = Path::new("/tmp/project with spaces/.celld-python/build");
        let command = builder_command(std::ffi::OsStr::new("/tools/pycelld"), config, output);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(command.get_program(), "/tools/pycelld");
        assert_eq!(
            args,
            [
                std::ffi::OsStr::new("build"),
                config.as_os_str(),
                std::ffi::OsStr::new("--out"),
                output.as_os_str()
            ]
        );
    }
}
