//! Built-in Cloudflare Python worker compiler. Runtime assets are embedded in
//! the binary; deployment and request handling never invoke a language CLI.
use crate::build_hooks::BundleOutput;
use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use flate2::read::GzDecoder;
use pep508_rs::{MarkerEnvironment, MarkerEnvironmentBuilder, Requirement, VersionOrUrl};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::path::Path;

const RUNTIME: &[u8] = include_bytes!(concat!(env!("CELLD_PYTHON_RUNTIME"), "/runtime.js.gz"));
const CORE: &[u8] = include_bytes!(concat!(env!("CELLD_PYTHON_RUNTIME"), "/core.wasm.gz"));
const CATALOG: &[u8] = include_bytes!(concat!(env!("CELLD_PYTHON_RUNTIME"), "/catalog.json.gz"));
const PYODIDE: &str = "314.0.6";

pub(crate) fn is_python(entry: &str) -> bool {
    Path::new(entry).extension().is_some_and(|ext| ext == "py")
}

fn inflate(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    GzDecoder::new(bytes).read_to_end(&mut output)?;
    Ok(output)
}

fn sources(
    root: &Path,
    directory: &Path,
    output: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some(
                ".celld"
                    | ".celld-python"
                    | ".venv"
                    | ".git"
                    | "__pycache__"
                    | "node_modules"
                    | "target"
            )
        ) {
            continue;
        }
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() {
            bail!(
                "Python source symlinks are not supported: {}",
                path.display()
            );
        }
        if kind.is_dir() {
            sources(root, &path, output)?;
        } else if path.extension().is_some_and(|ext| ext == "py") {
            let name = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            if name == "workers.py"
                || name.starts_with("workers/")
                || name.starts_with("_celld_")
                || name == "_cloudflare_compat_flags.py"
            {
                bail!("Python sources cannot shadow runtime modules: {name}");
            }
            let code = std::fs::read_to_string(&path)?;
            ruff_python_parser::parse_module(&code)
                .map_err(|error| anyhow!("Python syntax error in {}: {error}", path.display()))?;
            output.insert(name, code);
        }
    }
    Ok(())
}

fn requirements(root: &Path) -> anyhow::Result<Vec<String>> {
    let path = root.join("pyproject.toml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let project: toml::Value = toml::from_str(&std::fs::read_to_string(path)?)?;
    let Some(requirements) = project.get("project").and_then(|v| v.get("dependencies")) else {
        return Ok(Vec::new());
    };
    requirements
        .as_array()
        .context("project.dependencies must be an array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .context("Python dependency must be a string")
        })
        .collect()
}

fn resolve_packages(
    catalog: &Value,
    requirements: Vec<String>,
) -> anyhow::Result<Map<String, Value>> {
    let info = &catalog["info"];
    let python = info["python"].as_str().context("Python catalog version")?;
    let short = python.rsplit_once('.').context("Python version")?.0;
    let environment: MarkerEnvironment = MarkerEnvironmentBuilder {
        implementation_name: "cpython",
        implementation_version: python,
        os_name: "posix",
        platform_machine: "wasm32",
        platform_python_implementation: "CPython",
        platform_release: "",
        platform_system: "Emscripten",
        platform_version: "",
        python_full_version: python,
        python_version: short,
        sys_platform: "emscripten",
    }
    .try_into()?;
    let available: BTreeMap<String, &Value> = catalog["packages"]
        .as_object()
        .context("Python package catalog")?
        .iter()
        .map(|(name, package)| (name.replace(['_', '.'], "-").to_lowercase(), package))
        .collect();
    let mut pending: VecDeque<String> = requirements.into();
    let mut selected = Map::new();
    while let Some(text) = pending.pop_front() {
        let requirement: Requirement = text
            .parse()
            .with_context(|| format!("parse Python requirement {text:?}"))?;
        if !requirement.evaluate_markers(&environment, &[]) {
            continue;
        }
        if !requirement.extras.is_empty() {
            bail!("Python package extras require a compiler extension: {text}");
        }
        let name = requirement.name.to_string();
        let package = if name == "workers-runtime-sdk" {
            json!({"version":"1.8.3"})
        } else {
            available.get(&name).copied().cloned()
                .with_context(|| format!("{name} is not in the pinned Pyodide catalog; use a compiler hook for custom wheels"))?
        };
        match requirement.version_or_url {
            Some(VersionOrUrl::VersionSpecifier(spec)) => {
                let version = package["version"]
                    .as_str()
                    .context("package version")?
                    .parse()?;
                if !spec.contains(&version) {
                    bail!(
                        "{text} conflicts with bundled {name}=={}",
                        package["version"]
                    );
                }
            }
            Some(VersionOrUrl::Url(_)) => {
                bail!("URL Python requirements require a compiler extension: {text}")
            }
            None => {}
        }
        if name == "workers-runtime-sdk" || selected.contains_key(&name) {
            continue;
        }
        if let Some(dependencies) = package["depends"].as_array() {
            for dep in dependencies {
                pending.push_back(dep.as_str().context("catalog dependency")?.to_string());
            }
        }
        selected.insert(name, package);
    }
    Ok(selected)
}

fn package_assets(
    root: &Path,
    packages: &Map<String, Value>,
) -> anyhow::Result<Map<String, Value>> {
    let cache = root.join(".celld/python-cache");
    let mut assets = Map::new();
    for (name, package) in packages {
        let file_name = package["file_name"].as_str().context("package filename")?;
        let expected = package["sha256"].as_str().context("package checksum")?;
        let path = cache.join(expected).join(file_name);
        let bytes = if path.exists() {
            std::fs::read(&path)?
        } else {
            let url = format!("https://cdn.jsdelivr.net/pyodide/v{PYODIDE}/full/{file_name}");
            // build() may run inside celld's async CLI. This dedicated thread
            // owns its downloader runtime; no nested runtime or shell process.
            let bytes = std::thread::spawn(move || -> anyhow::Result<Vec<u8>> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let client = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(60))
                        .build()?;
                    Ok(client
                        .get(url)
                        .send()
                        .await?
                        .error_for_status()?
                        .bytes()
                        .await?
                        .to_vec())
                })
            })
            .join()
            .map_err(|_| anyhow!("Python package downloader panicked"))??;
            if format!("{:x}", Sha256::digest(&bytes)) != expected {
                bail!("Python package checksum mismatch: {name}");
            }
            std::fs::create_dir_all(path.parent().unwrap())?;
            let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            std::io::Write::write_all(&mut temp, &bytes)?;
            temp.persist(&path)?;
            bytes
        };
        if format!("{:x}", Sha256::digest(&bytes)) != expected {
            bail!("Cached Python package checksum mismatch: {name}");
        }
        assets.insert(
            format!("/packages/{file_name}"),
            Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        );
    }
    Ok(assets)
}

fn replace_json(template: &mut String, marker: &str, value: &Value) -> anyhow::Result<()> {
    let needle = serde_json::to_string(marker)?;
    if template.matches(&needle).count() != 1 {
        bail!("Embedded Python template marker mismatch: {marker}");
    }
    *template = template.replace(
        &needle,
        &serde_json::to_string(&serde_json::to_string(value)?)?,
    );
    Ok(())
}

pub(crate) fn bundle(root: &Path, entry: &str, metadata: &Value) -> anyhow::Result<BundleOutput> {
    if !metadata["compatibility_flags"]
        .as_array()
        .is_some_and(|flags| flags.iter().any(|flag| flag == "python_workers"))
    {
        bail!("Python entrypoints require compatibility_flags: [\"python_workers\"]");
    }
    let path = root.join(entry);
    let source_root = path.parent().context("Python entrypoint parent")?;
    let module = path
        .file_stem()
        .and_then(|name| name.to_str())
        .context("Python module name")?;
    if module.is_empty()
        || !module
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    {
        bail!("Python main must have an importable module filename");
    }
    let mut code = BTreeMap::new();
    sources(source_root, source_root, &mut code)?;
    let catalog: Value = serde_json::from_slice(&inflate(CATALOG)?)?;
    let packages = resolve_packages(&catalog, requirements(root)?)?;
    let assets = package_assets(root, &packages)?;
    let manifest = json!({"entrypoint":module,"sources":code,"lock":{"info":catalog["info"],"packages":packages}});
    let mut template = String::from_utf8(inflate(RUNTIME)?)?;
    replace_json(&mut template, "__CELLD_PYTHON_MANIFEST__", &manifest)?;
    replace_json(
        &mut template,
        "__CELLD_PYTHON_PACKAGES__",
        &Value::Object(assets),
    )?;
    Ok(BundleOutput {
        bundle: template.into_bytes(),
        wasm: vec![("pyodide.asm.wasm".into(), inflate(CORE)?)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_wasm_versions_and_target_markers_without_host_python() {
        let catalog = json!({"info":{"python":"3.14.2"},"packages":{
            "numpy":{"version":"2.4.1","depends":[]},
            "pydantic":{"version":"2.12.5","depends":["pydantic-core"]},
            "pydantic-core":{"version":"2.41.5","depends":[]}
        }});
        let packages = resolve_packages(
            &catalog,
            vec![
                "numpy>=2".into(),
                "pydantic<3".into(),
                "absent; sys_platform == 'linux'".into(),
            ],
        )
        .unwrap();
        assert_eq!(packages.len(), 3);
        assert!(resolve_packages(&catalog, vec!["numpy<2".into()]).is_err());
        assert!(resolve_packages(&catalog, vec!["absent".into()]).is_err());
        assert!(resolve_packages(&catalog, vec!["numpy[extra]".into()]).is_err());
    }
    #[test]
    fn template_replacement_preserves_source_quotes_and_unicode() {
        let mut source = "const value=JSON.parse(\"MARKER\");".to_string();
        replace_json(&mut source, "MARKER", &json!({"code":"hello \"世界\"\n"})).unwrap();
        assert!(!source.contains("MARKER"));
        assert!(replace_json(&mut source, "MARKER", &json!({})).is_err());
    }
}
