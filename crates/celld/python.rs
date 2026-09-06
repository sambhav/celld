//! Built-in Cloudflare Python compiler with separately provisioned artifacts.
use crate::build_hooks::{BundleOutput, SharedModule};
use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use pep508_rs::{MarkerEnvironment, MarkerEnvironmentBuilder, Requirement, VersionOrUrl};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

const PYODIDE: &str = "314.0.6";

pub(crate) fn is_python(entry: &str) -> bool {
    Path::new(entry).extension().is_some_and(|ext| ext == "py")
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
    if project
        .get("tool")
        .and_then(|v| v.get("celld"))
        .and_then(|v| v.get("wheels"))
        .is_some()
    {
        bail!("Custom Python wheels require a compiler extension");
    }
    if project
        .get("project")
        .and_then(|v| v.get("dynamic"))
        .and_then(toml::Value::as_array)
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.as_str() == Some("dependencies"))
        })
    {
        bail!("Dynamic Python dependencies require a compiler extension");
    }
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
            let bytes = crate::python_artifacts::download(url)?;
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

pub(crate) fn bundle(
    root: &Path,
    entry: &str,
    metadata: &Value,
    classes: &[String],
) -> anyhow::Result<BundleOutput> {
    if metadata["python_runtime"] == "monty" {
        return monty_bundle(root, entry, classes);
    }
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
    let mut artifacts = crate::python_artifacts::load(root)?;
    let catalog: Value = serde_json::from_slice(&artifacts.remove("catalog.json").unwrap())?;
    let packages = resolve_packages(&catalog, requirements(root)?)?;
    let assets = package_assets(root, &packages)?;
    let manifest = json!({"entrypoint":module,"sources":code,"lock":{"info":catalog["info"],"packages":packages}});
    let mut shared = Vec::new();
    for (name, bytes) in artifacts {
        let source = match name.as_str() {
            "_python_runtime.js" => SharedModule::EsModule(bytes),
            "pyodide.asm.wasm" => SharedModule::Wasm(bytes),
            _ => SharedModule::Text(bytes),
        };
        shared.push((name, source));
    }
    let mut imports = String::new();
    let mut entries = Vec::new();
    for (i, (path, data)) in assets.into_iter().enumerate() {
        let bytes = data
            .as_str()
            .context("Python package asset")?
            .as_bytes()
            .to_vec();
        let name = format!("python-{}.b64", crate::python_artifacts::digest(&bytes));
        imports.push_str(&format!("import package{i} from './{name}';\n"));
        entries.push(format!("{}:package{i}", serde_json::to_string(&path)?));
        shared.push((name, SharedModule::Text(bytes)));
    }
    // Only app sources, declarations and imports live in the app entrypoint.
    // Runtime module bytes are shared in S3; initialization remains lazy.
    let factories = if classes.is_empty() {"createPythonWorker"} else {"createPythonWorker, createPythonObject"};
    let mut bundle = format!("{imports}import {{{factories}}} from './_python_runtime.js';\nexport default createPythonWorker({},{{assets:{{{}}}}});\n", serde_json::to_string(&manifest)?, entries.join(","));
    for class in classes {
        if !class
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
        {
            bail!("invalid Python class name");
        }
        let entry_source = std::fs::read_to_string(root.join(entry))?;
        let parsed =
            ruff_python_parser::parse_module(&entry_source).map_err(|e| anyhow!(e.to_string()))?;
        let definition = parsed
            .syntax()
            .body
            .iter()
            .find_map(|stmt| match stmt {
                ruff_python_ast::Stmt::ClassDef(c) if c.name.as_str() == class => Some(c),
                _ => None,
            })
            .with_context(|| format!("Python durable class {class} is not defined in main"))?;
        let methods: Vec<_> = definition
            .body
            .iter()
            .filter_map(|stmt| match stmt {
                ruff_python_ast::Stmt::FunctionDef(f) if !f.name.starts_with('_') => {
                    Some(f.name.to_string())
                }
                _ => None,
            })
            .collect();
        let options = format!("{{assets:{{{}}}}}", entries.join(","));
        bundle.push_str(&format!(
            "export const {class} = createPythonObject({}, {}, {}, {options});\n",
            serde_json::to_string(&manifest)?,
            serde_json::to_string(class)?,
            serde_json::to_string(&methods)?
        ));
    }
    Ok(BundleOutput {
        bundle: bundle.into_bytes(),
        shared,
        ..Default::default()
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
}

fn monty_bundle(root: &Path, entry: &str, classes: &[String]) -> anyhow::Result<BundleOutput> {
    if !requirements(root)?.is_empty() {
        bail!("Monty cannot import third-party packages; select python_runtime: pyodide for Pydantic and WASM wheels");
    }
    let source = std::fs::read_to_string(root.join(entry))?;
    if source.len() > 256 * 1024 {
        bail!("Monty source exceeds 256 KiB");
    }
    let module = celld_monty::exports::Module::compile(&source).map_err(anyhow::Error::msg)?;
    let encoded = serde_json::to_string(&source)?;
    let mut bundle = format!("import {{createMontyWorker,createMontyObject}} from './_monty_runtime.js';\nconst source={encoded};\nexport default createMontyWorker(source,{});\n",module.manifest());
    for class in classes {
        let module = celld_monty::exports::Module::compile_class(&source, class)
            .map_err(anyhow::Error::msg)?;
        bundle.push_str(&format!(
            "export const {class} = createMontyObject(source,{},{});\n",
            module.manifest(),
            serde_json::to_string(class)?
        ));
    }
    Ok(BundleOutput {
        bundle: bundle.into_bytes(),
        shared: vec![(
            "_monty_runtime.js".into(),
            SharedModule::EsModule(include_bytes!("python/monty.mjs").to_vec()),
        )],
        ..Default::default()
    })
}
