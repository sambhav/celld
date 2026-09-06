//! Artifact provisioning is a control-plane operation. Servers load shared
//! modules from their deployment bucket, never from this build-time mirror.
use anyhow::{bail, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const FILES: &[&str] = &[
    "_python_runtime.js",
    "pyodide.asm.wasm",
    "python-stdlib.b64",
    "python-snapshot.b64",
    "catalog.json",
];

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    abi: String,
    pyodide: String,
    workers_sdk: String,
    files: BTreeMap<String, File>,
}
#[derive(Deserialize)]
struct File {
    bytes: usize,
    sha256: String,
    kind: Option<String>,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("celld")
}

pub(crate) fn download(url: String) -> anyhow::Result<Vec<u8>> {
    // build() can be called within the async CLI, so the downloader owns its
    // own thread/runtime instead of nesting a blocking runtime or shelling out.
    std::thread::spawn(move || -> anyhow::Result<Vec<u8>> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()?;
                let mut response = client.get(url).send().await?.error_for_status()?;
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await? {
                    anyhow::ensure!(
                        bytes.len() + chunk.len() <= MAX_FILE_BYTES,
                        "Python artifact exceeds 64 MiB"
                    );
                    bytes.extend_from_slice(&chunk);
                }
                Ok(bytes)
            })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("Python artifact downloader panicked"))?
}

fn write_cache(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::create_dir_all(path.parent().context("artifact cache directory")?)?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    std::io::Write::write_all(&mut temp, bytes)?;
    temp.persist(path)?;
    Ok(())
}

pub(crate) fn load(root: &Path) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let project_path = root.join("pyproject.toml");
    let project: toml::Value = if project_path.exists() {
        toml::from_str(&std::fs::read_to_string(project_path)?)?
    } else {
        toml::Value::Table(Default::default())
    };
    let setting = project
        .get("tool")
        .and_then(|v| v.get("celld"))
        .and_then(|v| v.get("python-runtime"));
    let (location, expected) = if let Some(setting) = setting {
        let table = setting
            .as_table()
            .context("tool.celld.python-runtime must be a table")?;
        anyhow::ensure!(
            table
                .keys()
                .all(|key| matches!(key.as_str(), "manifest" | "sha256")),
            "Unknown python-runtime option"
        );
        (
            setting
                .get("manifest")
                .and_then(toml::Value::as_str)
                .context("python-runtime.manifest is required")?
                .to_string(),
            Some(
                setting
                    .get("sha256")
                    .and_then(toml::Value::as_str)
                    .context("python-runtime.sha256 is required")?
                    .to_string(),
            ),
        )
    } else if let Some(directory) = std::env::var_os("CELLD_PYTHON_RUNTIME") {
        (
            PathBuf::from(directory)
                .join("runtime.json")
                .to_string_lossy()
                .into_owned(),
            None,
        )
    } else {
        let exe = std::env::current_exe()?;
        let parent = exe.parent().context("executable directory")?;
        let mut candidates = vec![
            parent.join("python-runtime/runtime.json"),
            cache_root().join("python-runtime/runtime.json"),
        ];
        if parent.file_name().is_some_and(|name| name == "deps") {
            candidates.insert(
                0,
                parent.parent().unwrap().join("python-runtime/runtime.json"),
            );
        }
        let path = candidates.into_iter().find(|path| path.is_file()).context(
            "Python runtime artifact not installed. Place python-runtime beside celld, set CELLD_PYTHON_RUNTIME, or pin tool.celld.python-runtime.manifest and sha256 to an HTTPS mirror. See examples/python/README.md")?;
        (path.to_string_lossy().into_owned(), None)
    };
    if let Some(expected) = &expected {
        anyhow::ensure!(
            valid_digest(expected),
            "Python runtime manifest requires a full lowercase SHA-256"
        );
    }
    let remote = location.starts_with("https://");
    anyhow::ensure!(
        remote || !location.contains("://"),
        "Python runtime mirror must use HTTPS"
    );
    let directory = if remote {
        cache_root().join("python-runtimes").join(
            expected
                .as_ref()
                .context("Remote runtime manifest must be pinned by SHA-256")?,
        )
    } else {
        root.join(&location)
            .parent()
            .context("runtime manifest directory")?
            .to_path_buf()
    };
    let manifest_path = if remote {
        directory.join("runtime.json")
    } else {
        root.join(&location)
    };
    let manifest_bytes = if manifest_path.exists() {
        std::fs::read(&manifest_path)?
    } else if remote {
        let bytes = download(location.clone())?;
        anyhow::ensure!(
            Some(digest(&bytes)).as_ref() == expected.as_ref(),
            "Python runtime manifest checksum mismatch"
        );
        write_cache(&manifest_path, &bytes)?;
        bytes
    } else {
        bail!(
            "Missing Python runtime manifest: {}",
            manifest_path.display()
        )
    };
    if let Some(expected) = expected {
        anyhow::ensure!(
            digest(&manifest_bytes) == expected,
            "Python runtime manifest checksum mismatch"
        );
    }
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("decode Python runtime manifest")?;
    anyhow::ensure!(
        manifest.schema_version == 1
            && manifest.abi == "celld-python-v1"
            && manifest.pyodide == "314.0.6"
            && manifest.workers_sdk == "1.8.3",
        "Unsupported Python runtime ABI/version"
    );
    anyhow::ensure!(
        manifest.files.len() == FILES.len()
            && FILES.iter().all(|name| manifest.files.contains_key(*name)),
        "Python runtime manifest file set does not match ABI"
    );
    let base_url = if remote {
        Some(reqwest::Url::parse(&location)?)
    } else {
        None
    };
    let mut output = BTreeMap::new();
    for (name, file) in manifest.files {
        let kind = match name.as_str() {
            "_python_runtime.js" => Some("esmodule"),
            "pyodide.asm.wasm" => Some("wasm"),
            _ => None,
        };
        anyhow::ensure!(
            file.kind.as_deref() == kind
                && valid_digest(&file.sha256)
                && file.bytes <= MAX_FILE_BYTES,
            "Invalid Python artifact descriptor: {name}"
        );
        let path = directory.join(&name);
        let bytes = if path.exists() {
            std::fs::read(&path)?
        } else if let Some(base) = &base_url {
            let bytes = download(base.join(&name)?.to_string())?;
            anyhow::ensure!(
                bytes.len() == file.bytes && digest(&bytes) == file.sha256,
                "Python artifact checksum mismatch: {name}"
            );
            write_cache(&path, &bytes)?;
            bytes
        } else {
            bail!("Missing Python runtime artifact: {}", path.display())
        };
        anyhow::ensure!(
            bytes.len() == file.bytes && digest(&bytes) == file.sha256,
            "Python artifact checksum mismatch: {name}"
        );
        output.insert(name, bytes);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let artifacts = root.path().join("runtime");
        std::fs::create_dir(&artifacts).unwrap();
        let mut files = serde_json::Map::new();
        for name in FILES {
            let kind = match *name {
                "_python_runtime.js" => Some("esmodule"),
                "pyodide.asm.wasm" => Some("wasm"),
                _ => None,
            };
            std::fs::write(artifacts.join(name), b"fixture").unwrap();
            files.insert(
                name.to_string(),
                serde_json::json!({"bytes":7,"sha256":digest(b"fixture"),"kind":kind}),
            );
        }
        let manifest = serde_json::to_vec(&serde_json::json!({"schema_version":1,"abi":"celld-python-v1","pyodide":"314.0.6","workers_sdk":"1.8.3","files":files})).unwrap();
        std::fs::write(artifacts.join("runtime.json"), &manifest).unwrap();
        std::fs::write(
            root.path().join("pyproject.toml"),
            format!(
                "[tool.celld.python-runtime]\nmanifest='runtime/runtime.json'\nsha256='{}'\n",
                digest(&manifest)
            ),
        )
        .unwrap();
        root
    }
    #[test]
    fn pinned_artifacts_are_loaded_offline_and_every_file_is_verified() {
        let root = fixture();
        assert_eq!(load(root.path()).unwrap().len(), FILES.len());
        std::fs::write(root.path().join("runtime/python-stdlib.b64"), b"damaged").unwrap();
        let error = load(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("checksum mismatch: python-stdlib.b64"),
            "{error}"
        );
    }
    #[test]
    fn modified_manifest_is_rejected_before_using_its_files() {
        let root = fixture();
        std::fs::write(root.path().join("runtime/runtime.json"), b"{}").unwrap();
        assert!(load(root.path())
            .unwrap_err()
            .to_string()
            .contains("manifest checksum mismatch"));
    }
    #[test]
    fn remote_manifests_must_be_https_and_pinned_before_any_download() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("pyproject.toml");
        std::fs::write(
            &project,
            "[tool.celld.python-runtime]\nmanifest='https://never-contact.invalid/runtime.json'\n",
        )
        .unwrap();
        assert!(load(root.path())
            .unwrap_err()
            .to_string()
            .contains("sha256 is required"));
        std::fs::write(&project, format!("[tool.celld.python-runtime]\nmanifest='http://never-contact.invalid/runtime.json'\nsha256='{}'\n", "0".repeat(64))).unwrap();
        assert!(load(root.path())
            .unwrap_err()
            .to_string()
            .contains("must use HTTPS"));
    }
}
