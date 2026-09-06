use celld::build_hooks::{BuildHooks, BuildRequest, BundleOutput};
use celld::deploy::{build, build_with_hooks, Options};
use std::sync::atomic::{AtomicUsize, Ordering};

fn options(config: std::path::PathBuf) -> Options {
    Options {
        config: Some(config),
        bucket: None,
        endpoint: None,
        region: None,
        dry_run: true,
        json: false,
    }
}
struct Compiler(AtomicUsize);
impl BuildHooks for Compiler {
    fn bundle(&self, request: &BuildRequest<'_>) -> anyhow::Result<Option<BundleOutput>> {
        assert_eq!(request.entrypoint, "source.custom");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some(BundleOutput {
            bundle: b"export default {fetch(){return new Response('hook')}};".to_vec(),
            wasm: vec![],
            ..Default::default()
        }))
    }
    fn finish(&self, output: &mut BundleOutput) -> anyhow::Result<()> {
        output.bundle.extend_from_slice(b"// transformed");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[test]
fn registered_compiler_and_transform_run_before_hashing() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source.custom"), "not JavaScript").unwrap();
    let config = root.path().join("wrangler.json");
    std::fs::write(&config, r#"{"name":"hook","main":"source.custom"}"#).unwrap();
    let hook = Compiler(AtomicUsize::new(0));
    let first = build_with_hooks(&options(config.clone()), &hook).unwrap();
    let second = build_with_hooks(&options(config), &hook).unwrap();
    assert_eq!(hook.0.load(Ordering::SeqCst), 4);
    assert_eq!(first.version, second.version);
    assert!(first.modules[0].1.ends_with(b"// transformed"));
}
#[test]
fn default_python_compiler_uses_shared_artifacts_and_rejects_bad_syntax() {
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("worker.py");
    std::fs::write(&entry, "from workers import WorkerEntrypoint, Response\nclass Default(WorkerEntrypoint):\n async def fetch(self, request):\n  return Response('hello')\n").unwrap();
    let config = root.path().join("wrangler.json");
    std::fs::write(
        &config,
        r#"{"name":"python","main":"worker.py","compatibility_flags":["python_workers"]}"#,
    )
    .unwrap();
    let built = build(&options(config.clone())).unwrap();
    assert!(built
        .modules
        .iter()
        .any(|(name, bytes)| name.ends_with(".wasm") && bytes.starts_with(b"\0asm")));
    assert!(!String::from_utf8_lossy(&built.modules[0].1).contains("__CELLD_PYTHON_MANIFEST__"));
    assert!(
        built.modules[0].1.len() < 4096,
        "hello world entry must not contain runtime bytes"
    );
    assert!(built
        .manifest
        .required_features
        .iter()
        .any(|feature| feature == "shared-modules-v1"));
    celld::protocol::validate_shared_modules(&built.manifest).unwrap();
    let mut ungated = built.manifest.clone();
    ungated.required_features.clear();
    assert!(celld::protocol::validate_shared_modules(&ungated).is_err());
    let shared = |built: &celld::deploy::Built| {
        built
            .manifest
            .modules
            .iter()
            .filter(|module| module.shared)
            .map(|module| (module.name.clone(), module.sha256.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(shared(&built).len(), 4);
    let code = std::fs::read_to_string(&entry).unwrap();
    std::fs::write(&entry, code.replace("hello", "updated")).unwrap();
    let updated = build(&options(config.clone())).unwrap();
    assert_ne!(built.version, updated.version);
    assert_eq!(
        shared(&built),
        shared(&updated),
        "source edits must reuse exact runtime hashes"
    );
    std::fs::write(entry, "invalid Python!\n").unwrap();
    assert!(build(&options(config))
        .err()
        .unwrap()
        .to_string()
        .contains("Python syntax error"));
}
