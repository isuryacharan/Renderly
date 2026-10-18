//! Content hashes for pack items (native only: needs `sha2` and the filesystem).
//!
//! An item's hash is SHA-256 over the item's manifest and every file it references, each named
//! by its path relative to the item folder. Per-file digests are cached by `(path, mtime,
//! size)` (docs/effect-pack-format.md, Decision 2), so reloading an unchanged pack does not
//! hash it again. The cache trusts mtime and size: a rewrite that keeps both looks unchanged
//! until the process restarts, which is the trade the design accepts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// A SHA-256 digest.
pub type Digest32 = [u8; 32];

#[derive(Clone, Copy)]
struct Cached {
    mtime: Option<SystemTime>,
    len: u64,
    digest: Digest32,
}

fn cache() -> MutexGuard<'static, HashMap<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> Digest32 {
    Sha256::digest(bytes).into()
}

/// The digest of the file at `path` whose contents the caller has just read as `bytes`.
/// Reuses the cached digest when the file's mtime and length are unchanged.
pub fn file_digest(path: &Path, bytes: &[u8]) -> Digest32 {
    let meta = std::fs::metadata(path).ok();
    let mtime = meta.as_ref().and_then(|m| m.modified().ok());
    let len = bytes.len() as u64;
    if let Some(hit) = cache().get(path) {
        if hit.mtime == mtime && hit.len == len && mtime.is_some() {
            return hit.digest;
        }
    }
    let digest = sha256(bytes);
    cache().insert(path.to_path_buf(), Cached { mtime, len, digest });
    digest
}

/// The item hash: SHA-256 over `(relative path, file digest)` pairs in path order, as lowercase
/// hex. The pairs must cover the item manifest and every file it references.
pub fn item_hash(entries: &[(String, Digest32)]) -> String {
    let mut sorted: Vec<&(String, Digest32)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (name, digest) in sorted {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update(digest);
    }
    to_hex(&hasher.finalize())
}

/// Lowercase hex of `bytes`.
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_hash_is_order_independent_and_content_sensitive() {
        let a = ("effect.json".to_string(), sha256(b"{}"));
        let b = ("grain.wgsl".to_string(), sha256(b"fn"));
        let ab = item_hash(&[a.clone(), b.clone()]);
        assert_eq!(ab, item_hash(&[b.clone(), a.clone()]));
        assert_eq!(ab.len(), 64);
        let changed = ("grain.wgsl".to_string(), sha256(b"fn2"));
        assert_ne!(ab, item_hash(&[a, changed]));
    }
}
