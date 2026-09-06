//! In-process compiler extension points. celld never discovers or executes an
//! external language CLI through these hooks. A registered extension can replace
//! the built-in Python/JS compiler and transform its output before hashing it.
use std::path::{Path, PathBuf};

pub struct BuildRequest<'a> {
    pub config: &'a Path,
    pub root: &'a Path,
    pub entrypoint: &'a str,
}

/// A bundled ES module and its statically imported compiled-WASM siblings.
pub struct BundleOutput {
    pub bundle: Vec<u8>,
    pub wasm: Vec<(String, Vec<u8>)>,
}

pub trait BuildHooks: Send + Sync {
    /// Runs on every build, including reload. Errors leave publication untouched.
    fn prepare(&self, config: &Path) -> anyhow::Result<PathBuf> {
        Ok(config.to_path_buf())
    }
    /// Return None to use the built-in Python or JavaScript compiler.
    fn bundle(&self, _request: &BuildRequest<'_>) -> anyhow::Result<Option<BundleOutput>> {
        Ok(None)
    }
    /// Runs before the deployment identity is computed or anything is published.
    fn finish(&self, _output: &mut BundleOutput) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct NoBuildHooks;
impl BuildHooks for NoBuildHooks {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_hooks_leave_builtin_compilers_and_bytes_intact() {
        let config = Path::new("/project/wrangler.jsonc");
        let request = BuildRequest {
            config,
            root: Path::new("/project"),
            entrypoint: "src/worker.py",
        };
        let hooks = NoBuildHooks;
        assert_eq!(hooks.prepare(config).unwrap(), config);
        assert!(hooks.bundle(&request).unwrap().is_none());
        let mut output = BundleOutput {
            bundle: b"module".to_vec(),
            wasm: vec![("core.wasm".into(), vec![0, 97, 115, 109])],
        };
        hooks.finish(&mut output).unwrap();
        assert_eq!(output.bundle, b"module");
        assert_eq!(output.wasm[0].1, [0, 97, 115, 109]);
    }
}
