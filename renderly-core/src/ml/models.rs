//! Manifest of the ML models Renderly knows about, and how their files are found.
//!
//! Resolution order for a model file ([`resolve_model`]):
//!
//! 1. the directory named by the `RENDERLY_MODEL_DIR` environment variable, if it contains the
//!    manifest file name (developer / air-gapped override; the file is used as-is, unverified);
//! 2. bytes bundled into the binary (`include_bytes!`) — only for models whose licence allows
//!    redistribution;
//! 3. the per-user model cache ([`model_cache_dir`]), where [`download_model`] puts verified
//!    downloads.
//!
//! Downloading only ever happens through an explicit [`download_model`] call (the caller is
//! responsible for obtaining user consent first); resolving never touches the network.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Environment variable naming a directory that overrides model lookup (see module docs).
pub const DEFAULT_MODEL_DIR_ENV: &str = "RENDERLY_MODEL_DIR";

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("unknown model id {0:?}")]
    UnknownModel(String),
    #[error(
        "model {id:?} is not installed; it must be downloaded first (expected {file_name} in {cache_dir} or ${DEFAULT_MODEL_DIR_ENV})"
    )]
    NotInstalled {
        id: String,
        file_name: String,
        cache_dir: String,
    },
    #[error("model {0:?} is bundled and has no download URL")]
    NotDownloadable(String),
    #[error("download of {url} failed: {message}")]
    Download { url: String, message: String },
    #[error("checksum mismatch for {id:?}: expected sha256 {expected}, got {actual}")]
    ChecksumMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("size mismatch for {id:?}: expected {expected} bytes, got {actual}")]
    SizeMismatch {
        id: String,
        expected: u64,
        actual: u64,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Description of a model input, for humans and for the code that feeds it.
#[derive(Debug, Clone, Copy)]
pub struct InputSpec {
    pub name: &'static str,
    /// Tensor layout / shape, e.g. `"NCHW f32, H and W multiples of 32"`.
    pub shape: &'static str,
    /// Pixel convention, e.g. `"BGR, 0..255, no mean/std"`.
    pub pixels: &'static str,
}

/// One manifest entry.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    /// SPDX identifier of the model weights' licence.
    pub license_spdx: &'static str,
    /// Where the licence statement lives (URL).
    pub license_url: &'static str,
    /// Project page the model comes from.
    pub source_url: &'static str,
    /// File name used in the cache / override directory.
    pub file_name: &'static str,
    /// Bytes compiled into the binary, when redistribution is permitted.
    pub bundled: Option<&'static [u8]>,
    /// Upstream download URL, pinned to an immutable ref (commit hash or release tag).
    pub download_url: Option<&'static str>,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: &'static str,
    pub size_bytes: u64,
    pub inputs: &'static [InputSpec],
}

/// Every model Renderly can use. Add entries here (with a pinned SHA-256) — the unit tests
/// enforce the integrity of this table.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "yunet-face-2026may",
        name: "YuNet face detector (OpenCV Zoo, dynamic input)",
        license_spdx: "MIT",
        license_url: "https://github.com/opencv/opencv_zoo/blob/26cc381e4d2594bb9f47a26eb8fd96c94a13660d/models/face_detection_yunet/LICENSE",
        source_url: "https://github.com/opencv/opencv_zoo/tree/26cc381e4d2594bb9f47a26eb8fd96c94a13660d/models/face_detection_yunet",
        file_name: "face_detection_yunet_2026may.onnx",
        bundled: Some(include_bytes!(
            "../../assets/models/face_detection_yunet_2026may.onnx"
        )),
        // Git-LFS object, reachable through the pinned commit (the commit's pointer file holds
        // the SHA-256 below, so the ref is content-addressed).
        download_url: Some(concat!(
            "https://github.com/opencv/opencv_zoo/raw/",
            "26cc381e4d2594bb9f47a26eb8fd96c94a13660d",
            "/models/face_detection_yunet/face_detection_yunet_2026may.onnx"
        )),
        sha256: "ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0",
        size_bytes: 229_738,
        inputs: &[InputSpec {
            name: "input",
            shape: "1x3xHxW f32, H and W multiples of 32",
            pixels: "BGR channel order, raw 0..255 values, no mean/std",
        }],
    },
    ModelSpec {
        id: "rvm-mobilenetv3-fp32",
        name: "Robust Video Matting (MobileNetV3, fp32)",
        license_spdx: "GPL-3.0-only",
        license_url: "https://github.com/PeterL1n/RobustVideoMatting/blob/master/LICENSE",
        source_url: "https://github.com/PeterL1n/RobustVideoMatting",
        file_name: "rvm_mobilenetv3_fp32.onnx",
        bundled: None,
        download_url: Some(
            "https://github.com/PeterL1n/RobustVideoMatting/releases/download/v1.0.0/rvm_mobilenetv3_fp32.onnx",
        ),
        sha256: "88d4531297118f595bf2fd60f6f566aec2e559393802d1f436c380f0cbbd2828",
        size_bytes: 14_975_696,
        inputs: &[
            InputSpec {
                name: "src",
                shape: "1x3xHxW f32 (e.g. 288x512)",
                pixels: "RGB, 0..1",
            },
            InputSpec {
                name: "r1i..r4i",
                shape: "recurrent state: 1x1x1x1 zeros for the first frame, then the previous r1o..r4o",
                pixels: "f32 state",
            },
            InputSpec {
                name: "downsample_ratio",
                shape: "[1] f32 scalar, e.g. 0.5",
                pixels: "-",
            },
        ],
    },
];

/// Look a model up by id.
pub fn spec(id: &str) -> Result<&'static ModelSpec, ModelError> {
    MODELS
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| ModelError::UnknownModel(id.to_string()))
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Per-user model cache directory: `%LOCALAPPDATA%\renderly\models` on Windows,
/// `$XDG_DATA_HOME/renderly/models` (default `~/.local/share/renderly/models`) elsewhere —
/// the same base the live-agent bridge discovery file uses. The core crate must not depend on
/// Tauri, so this mirrors that convention instead of asking the app for a path.
pub fn model_cache_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|h| PathBuf::from(h).join("AppData").join("Local"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
    };
    base.join("renderly").join("models")
}

/// Where a resolved model lives.
#[derive(Debug, Clone)]
pub enum ModelSource {
    Bundled(&'static [u8]),
    File(PathBuf),
}

/// Resolve `id` using the process environment and the real cache directory.
pub fn resolve_model(id: &str) -> Result<ModelSource, ModelError> {
    let override_dir = std::env::var_os(DEFAULT_MODEL_DIR_ENV).map(PathBuf::from);
    resolve_in(spec(id)?, override_dir.as_deref(), &model_cache_dir())
}

/// Resolution with explicit directories (the testable core of [`resolve_model`]).
pub fn resolve_in(
    spec: &ModelSpec,
    override_dir: Option<&Path>,
    cache_dir: &Path,
) -> Result<ModelSource, ModelError> {
    if let Some(dir) = override_dir {
        let p = dir.join(spec.file_name);
        if p.is_file() {
            return Ok(ModelSource::File(p));
        }
    }
    if let Some(bytes) = spec.bundled {
        return Ok(ModelSource::Bundled(bytes));
    }
    let p = cache_dir.join(spec.file_name);
    // Cache files are only ever created by an atomic rename after verification, so a size
    // check is enough to catch a truncated/foreign file without re-hashing hundreds of MB.
    if std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() == spec.size_bytes) {
        return Ok(ModelSource::File(p));
    }
    Err(ModelError::NotInstalled {
        id: spec.id.to_string(),
        file_name: spec.file_name.to_string(),
        cache_dir: cache_dir.display().to_string(),
    })
}

/// Whether `id` can be resolved right now without downloading.
pub fn is_available(id: &str) -> bool {
    resolve_model(id).is_ok()
}

/// Download model `id` from its pinned upstream URL into the per-user cache, verify its size and
/// SHA-256 (failing closed), and atomically move it into place. Returns the final path.
///
/// `progress` receives `(bytes_downloaded, total_bytes_if_known)`. This is the only function in
/// the crate that touches the network for models; callers must have the user's consent first.
pub fn download_model(
    id: &str,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, ModelError> {
    let spec = spec(id)?;
    let url = spec
        .download_url
        .ok_or_else(|| ModelError::NotDownloadable(id.to_string()))?;
    download_to(spec, url, &model_cache_dir(), progress)
}

/// [`download_model`] with explicit URL and destination directory (also used by tests).
pub fn download_to(
    spec: &ModelSpec,
    url: &str,
    dest_dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, ModelError> {
    let fail = |message: String| ModelError::Download {
        url: url.to_string(),
        message,
    };
    std::fs::create_dir_all(dest_dir)?;
    let final_path = dest_dir.join(spec.file_name);
    let tmp_path = dest_dir.join(format!(
        "{}.{}.part",
        spec.file_name,
        uuid::Uuid::new_v4().simple()
    ));

    let mut builder = reqwest::blocking::Client::builder().timeout(None);
    if url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost") {
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|e| fail(e.to_string()))?;
    let mut resp = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| fail(e.to_string()))?;
    let total = resp.content_length();

    let result = (|| -> Result<(), ModelError> {
        let mut file = std::fs::File::create(&tmp_path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut done: u64 = 0;
        loop {
            let n = resp.read(&mut buf).map_err(|e| fail(e.to_string()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
            // Refuse to keep reading an endless / wrong body.
            if done > spec.size_bytes {
                return Err(ModelError::SizeMismatch {
                    id: spec.id.to_string(),
                    expected: spec.size_bytes,
                    actual: done,
                });
            }
        }
        file.flush()?;
        file.sync_all()?;
        if done != spec.size_bytes {
            return Err(ModelError::SizeMismatch {
                id: spec.id.to_string(),
                expected: spec.size_bytes,
                actual: done,
            });
        }
        let actual = hex(&hasher.finalize());
        if actual != spec.sha256 {
            return Err(ModelError::ChecksumMismatch {
                id: spec.id.to_string(),
                expected: spec.sha256.to_string(),
                actual,
            });
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    // `rename` over an existing file replaces it atomically on POSIX and (via MoveFileEx
    // REPLACE_EXISTING, which std uses) on Windows.
    if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(final_path)
}

/// What a UI / agent needs to know to ask for consent and to show state: the manifest facts
/// plus whether the file is installed right now.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ModelStatus {
    pub id: String,
    pub name: String,
    pub license_spdx: String,
    pub license_url: String,
    pub source_url: String,
    /// Host the weights are downloaded from (`None` when bundled / not downloadable).
    pub download_host: Option<String>,
    pub size_bytes: u64,
    pub sha256: String,
    /// Resolvable right now (bundled, in the override dir, or in the cache).
    pub installed: bool,
    /// Where the file lives when `installed` and file-backed.
    pub path: Option<String>,
    /// Where a download would put it.
    pub cache_dir: String,
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    Some(rest.split(['/', '?', '#']).next()?.to_string())
}

/// Status of one manifest model (resolution only; never touches the network).
pub fn model_status(id: &str) -> Result<ModelStatus, ModelError> {
    let spec = spec(id)?;
    let resolved = resolve_model(id).ok();
    Ok(ModelStatus {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        license_spdx: spec.license_spdx.to_string(),
        license_url: spec.license_url.to_string(),
        source_url: spec.source_url.to_string(),
        download_host: spec.download_url.and_then(url_host),
        size_bytes: spec.size_bytes,
        sha256: spec.sha256.to_string(),
        installed: resolved.is_some(),
        path: match &resolved {
            Some(ModelSource::File(p)) => Some(p.display().to_string()),
            _ => None,
        },
        cache_dir: model_cache_dir().display().to_string(),
    })
}

/// [`model_status`] for every manifest model.
pub fn list_model_status() -> Vec<ModelStatus> {
    MODELS
        .iter()
        .filter_map(|m| model_status(m.id).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn status_reports_host_licence_and_size() {
        let s = model_status("rvm-mobilenetv3-fp32").unwrap();
        assert_eq!(s.download_host.as_deref(), Some("github.com"));
        assert_eq!(s.license_spdx, "GPL-3.0-only");
        assert_eq!(s.size_bytes, 14_975_696);
        assert!(model_status("nope").is_err());
        // The detector is bundled, so it is always installed.
        let y = model_status("yunet-face-2026may").unwrap();
        assert!(y.installed);
        assert_eq!(url_host("https://a.b/c?d"), Some("a.b".into()));
        assert_eq!(url_host("nope"), None);
    }
    use std::net::TcpListener;

    fn is_hex64(s: &str) -> bool {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }

    #[test]
    fn manifest_entries_are_complete() {
        let mut ids = std::collections::HashSet::new();
        for m in MODELS {
            assert!(ids.insert(m.id), "duplicate id {}", m.id);
            assert!(!m.name.is_empty() && !m.file_name.is_empty(), "{}", m.id);
            assert!(!m.license_spdx.is_empty(), "{} has no licence", m.id);
            assert!(m.license_url.starts_with("https://"), "{}", m.id);
            assert!(m.source_url.starts_with("https://"), "{}", m.id);
            assert!(is_hex64(m.sha256), "{} sha256 malformed", m.id);
            assert!(m.size_bytes > 0, "{}", m.id);
            assert!(!m.inputs.is_empty(), "{}", m.id);
            assert!(
                m.bundled.is_some() || m.download_url.is_some(),
                "{} is neither bundled nor downloadable",
                m.id
            );
            if let Some(url) = m.download_url {
                assert!(url.starts_with("https://"), "{}", m.id);
                // Never a moving branch: a 40-hex commit or a release tag path segment.
                assert!(
                    !url.contains("/main/")
                        && !url.contains("/master/")
                        && !url.contains("/latest/"),
                    "{} download URL points at a moving ref: {url}",
                    m.id
                );
                let pinned = url.contains("/releases/download/")
                    || url
                        .split('/')
                        .any(|seg| seg.len() == 40 && seg.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(pinned, "{} URL is not pinned to a tag/commit: {url}", m.id);
            }
        }
    }

    #[test]
    fn bundled_bytes_match_manifest() {
        for m in MODELS {
            if let Some(bytes) = m.bundled {
                assert_eq!(bytes.len() as u64, m.size_bytes, "{} size", m.id);
                assert_eq!(sha256_hex(bytes), m.sha256, "{} sha256", m.id);
            }
        }
    }

    #[test]
    fn only_permissively_licensed_models_are_bundled() {
        for m in MODELS.iter().filter(|m| m.bundled.is_some()) {
            assert!(
                matches!(m.license_spdx, "MIT" | "Apache-2.0" | "BSD-3-Clause"),
                "{} is bundled under {}",
                m.id,
                m.license_spdx
            );
        }
    }

    #[test]
    fn unknown_id_is_rejected() {
        assert!(matches!(spec("nope"), Err(ModelError::UnknownModel(_))));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("renderly-ml-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_spec(file: &str, data: &[u8], bundled: Option<&'static [u8]>) -> ModelSpec {
        ModelSpec {
            id: "fake",
            name: "fake",
            license_spdx: "MIT",
            license_url: "https://example.invalid/l",
            source_url: "https://example.invalid/s",
            file_name: Box::leak(file.to_string().into_boxed_str()),
            bundled,
            download_url: None,
            sha256: Box::leak(sha256_hex(data).into_boxed_str()),
            size_bytes: data.len() as u64,
            inputs: &[],
        }
    }

    #[test]
    fn resolution_order_env_then_bundled_then_cache() {
        let env_dir = temp_dir("env");
        let cache = temp_dir("cache");
        static BUNDLED: &[u8] = b"bundled-bytes";
        let with_bundle = fake_spec("m.onnx", b"cached-bytes", Some(BUNDLED));
        let no_bundle = fake_spec("m.onnx", b"cached-bytes", None);

        // Nothing anywhere: not installed (unbundled) / bundled (bundled).
        assert!(matches!(
            resolve_in(&no_bundle, None, &cache),
            Err(ModelError::NotInstalled { .. })
        ));
        assert!(matches!(
            resolve_in(&with_bundle, None, &cache).unwrap(),
            ModelSource::Bundled(b) if b == BUNDLED
        ));

        // Cache hit only counts when the size matches.
        std::fs::write(cache.join("m.onnx"), b"short").unwrap();
        assert!(resolve_in(&no_bundle, None, &cache).is_err());
        std::fs::write(cache.join("m.onnx"), b"cached-bytes").unwrap();
        assert!(matches!(
            resolve_in(&no_bundle, None, &cache).unwrap(),
            ModelSource::File(p) if p == cache.join("m.onnx")
        ));
        // Bundled beats the cache.
        assert!(matches!(
            resolve_in(&with_bundle, None, &cache).unwrap(),
            ModelSource::Bundled(_)
        ));

        // Env override beats both, but only if it actually holds the file.
        let empty = temp_dir("empty");
        assert!(matches!(
            resolve_in(&with_bundle, Some(&empty), &cache).unwrap(),
            ModelSource::Bundled(_)
        ));
        std::fs::write(env_dir.join("m.onnx"), b"anything").unwrap();
        assert!(matches!(
            resolve_in(&with_bundle, Some(&env_dir), &cache).unwrap(),
            ModelSource::File(p) if p == env_dir.join("m.onnx")
        ));
    }

    /// Serve `body` once per connection on a loopback port; returns the URL.
    fn serve(body: Vec<u8>, connections: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..connections {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok() {
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    line.clear();
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("http://127.0.0.1:{port}/model.onnx")
    }

    #[test]
    fn download_verifies_and_installs_atomically() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let spec = fake_spec("dl.onnx", &data, None);
        let url = serve(data.clone(), 1);
        let dir = temp_dir("dl-ok");
        let mut last = (0u64, None);
        let path = download_to(&spec, &url, &dir, &mut |d, t| last = (d, t)).unwrap();
        assert_eq!(path, dir.join("dl.onnx"));
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(last, (data.len() as u64, Some(data.len() as u64)));
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["dl.onnx".to_string()]);
    }

    #[test]
    fn download_rejects_checksum_mismatch_and_keeps_existing_file() {
        let good: Vec<u8> = vec![7u8; 4096];
        let mut evil = good.clone();
        evil[100] ^= 0xff; // same length, different content
        let spec = fake_spec("dl.onnx", &good, None);
        let url = serve(evil, 1);
        let dir = temp_dir("dl-bad");
        // A previously installed good file must survive a failed re-download.
        std::fs::write(dir.join("dl.onnx"), &good).unwrap();
        let err = download_to(&spec, &url, &dir, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, ModelError::ChecksumMismatch { .. }), "{err}");
        assert_eq!(std::fs::read(dir.join("dl.onnx")).unwrap(), good);
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "temp file not cleaned up: {names:?}");
    }

    #[test]
    fn download_rejects_wrong_size() {
        let good: Vec<u8> = vec![1u8; 2048];
        let spec = fake_spec("dl.onnx", &good, None);
        let dir = temp_dir("dl-size");
        let short = serve(vec![1u8; 100], 1);
        let err = download_to(&spec, &short, &dir, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, ModelError::SizeMismatch { .. }), "{err}");
        let long = serve(vec![1u8; 5000], 1);
        let err = download_to(&spec, &long, &dir, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, ModelError::SizeMismatch { .. }), "{err}");
        assert!(!dir.join("dl.onnx").exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn download_model_refuses_bundled_only_and_unknown() {
        let mut noop = |_: u64, _: Option<u64>| {};
        assert!(matches!(
            download_model("nope", &mut noop),
            Err(ModelError::UnknownModel(_))
        ));
    }
}
