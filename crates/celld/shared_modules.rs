//! Immutable fleet-wide modules with a verified, disposable node disk cache.
use crate::bucket::Bucket;
use crate::protocol::ModuleRef;
use anyhow::Context;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const CACHE_BYTES: u64 = 1024 * 1024 * 1024;
fn key(module: &ModuleRef) -> anyhow::Result<String> {
    anyhow::ensure!(
        module.shared && crate::python_artifacts::valid_digest(&module.sha256),
        "Shared module requires a full lowercase SHA-256"
    );
    Ok(format!("modules/sha256/{}", module.sha256))
}
fn verify(module: &ModuleRef, bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        bytes.len() == module.bytes && format!("{:x}", Sha256::digest(bytes)) == module.sha256,
        "Shared module checksum/length mismatch: {}",
        module.name
    );
    Ok(())
}

pub(crate) async fn publish(
    bucket: &Bucket,
    module: &ModuleRef,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let key = key(module)?;
    verify(module, bytes)?;
    if let Some((size, hash)) = bucket.head_with_meta(&key, "sha256").await? {
        if size == bytes.len() as u64 && hash.as_deref() == Some(&module.sha256) {
            return Ok(());
        }
    }
    bucket
        .put_with_meta(&key, bytes.to_vec(), &[("sha256", &module.sha256)])
        .await
}

fn cache_directory() -> PathBuf {
    std::env::var_os("CELLD_MODULE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::python_artifacts::cache_root().join("modules-v1"))
}

// Atomic writes plus a soft 1 GiB cap; discard oldest writes first. Cache
// failures affect performance only. Every hit is verified before execution.
fn cache_write(directory: &Path, sha256: &str, bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() as u64 > CACHE_BYTES {
        return Ok(());
    }
    std::fs::create_dir_all(directory)?;
    let mut files = Vec::new();
    let mut size = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if !crate::python_artifacts::valid_digest(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        size += metadata.len();
        files.push((metadata.modified()?, metadata.len(), entry.path()));
    }
    files.sort_by_key(|file| file.0);
    for (_, length, path) in files {
        if size + bytes.len() as u64 <= CACHE_BYTES {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            size = size.saturating_sub(length);
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    std::io::Write::write_all(&mut temp, bytes)?;
    temp.persist(directory.join(sha256))?;
    Ok(())
}

pub(crate) async fn load(bucket: &Bucket, module: &ModuleRef) -> anyhow::Result<Bytes> {
    load_with_cache(bucket, module, &cache_directory()).await
}

async fn load_with_cache(
    bucket: &Bucket,
    module: &ModuleRef,
    directory: &Path,
) -> anyhow::Result<Bytes> {
    let key = key(module)?;
    let directory = directory.to_path_buf();
    let path = directory.join(&module.sha256);
    if let Ok(bytes) = tokio::fs::read(&path).await {
        if verify(module, &bytes).is_ok() {
            return Ok(bytes.into());
        }
        // Damaged local caches are disposable; recover from authoritative S3.
        let _ = tokio::fs::remove_file(&path).await;
    }
    let (bytes, _) = bucket
        .get(&key)
        .await?
        .with_context(|| format!("Missing shared module: {key}"))?;
    verify(module, &bytes)?;
    let cached = bytes.clone();
    let hash = module.sha256.clone();
    if let Err(error) =
        tokio::task::spawn_blocking(move || cache_write(&directory, &hash, &cached)).await?
    {
        tracing::warn!(%error, "shared module disk cache unavailable");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn module() -> ModuleRef {
        ModuleRef {
            name: "test.txt".into(),
            bytes: 5,
            sha256: format!("{:x}", Sha256::digest(b"hello")),
            kind: None,
            shared: true,
        }
    }
    #[test]
    fn shared_references_reject_truncated_hashes_and_corrupt_bytes() {
        let mut module = module();
        assert!(key(&module).unwrap().starts_with("modules/sha256/"));
        verify(&module, b"hello").unwrap();
        assert!(verify(&module, b"HELLO").is_err());
        module.sha256.truncate(16);
        assert!(key(&module).is_err());
        module.sha256 = "../escape".into();
        assert!(key(&module).is_err());
    }
    #[test]
    fn cache_writes_are_atomic_and_reusable() {
        let root = tempfile::tempdir().unwrap();
        let module = module();
        cache_write(root.path(), &module.sha256, b"hello").unwrap();
        cache_write(root.path(), &module.sha256, b"hello").unwrap();
        verify(
            &module,
            &std::fs::read(root.path().join(&module.sha256)).unwrap(),
        )
        .unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn reuse_cached_modules_and_recover_corruption_without_executing_bad_bytes() {
        let root = tempfile::tempdir().unwrap();
        let bucket = Bucket::open_dev(&root.path().join("objects.sqlite3")).unwrap();
        let cache = root.path().join("cache");
        let module = module();
        publish(&bucket, &module, b"hello").await.unwrap();
        let first = bucket.head(&key(&module).unwrap()).await.unwrap().unwrap();
        publish(&bucket, &module, b"hello").await.unwrap();
        assert_eq!(
            first,
            bucket.head(&key(&module).unwrap()).await.unwrap().unwrap()
        );
        assert_eq!(
            load_with_cache(&bucket, &module, &cache).await.unwrap(),
            b"hello"[..]
        );
        // Damaged bucket data cannot affect an already verified cache hit.
        bucket
            .put(&key(&module).unwrap(), b"wrong".to_vec())
            .await
            .unwrap();
        assert_eq!(
            load_with_cache(&bucket, &module, &cache).await.unwrap(),
            b"hello"[..]
        );
        std::fs::write(cache.join(&module.sha256), b"WRONG").unwrap();
        assert!(load_with_cache(&bucket, &module, &cache).await.is_err());
        publish(&bucket, &module, b"hello").await.unwrap();
        assert_eq!(
            load_with_cache(&bucket, &module, &cache).await.unwrap(),
            b"hello"[..]
        );
        verify(&module, &std::fs::read(cache.join(&module.sha256)).unwrap()).unwrap();
    }
}
