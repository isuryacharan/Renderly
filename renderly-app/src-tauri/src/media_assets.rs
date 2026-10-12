//! Background thumbnail-strip + waveform generation for imported media, cached on disk.
//!
//! Decoupled from the edit path entirely: generation runs as a plain tokio task (not
//! under `AppState::edit_lock`), so importing media or opening a project with many items
//! never blocks — or even slows down — `apply_command`/`undo`/`redo`. Results are cached
//! by `(path, mtime)` so re-opening the same project doesn't regenerate anything, and
//! delivered to the frontend via `media:thumbnails-ready` / `media:waveform-ready` events
//! (or synchronously via `get_media_assets` if a cache entry already exists).
//!
//! ## Cache location: project-local sidecar directory
//!
//! Generated assets live next to the project file, not in a single global app-cache
//! directory — otherwise every "reopen an existing project" is a coin flip on whether the
//! OS has since cleared the app cache, and two projects sharing a media file fight over
//! the same global entry. The convention (mirrors `project_thumb_path` in `lib.rs`, which
//! does the same next-to-the-project trick for the home-screen gallery thumb): for a
//! project file `Foo.renderly.json`, its sidecar directory is `Foo.renderly-assets/` in
//! the same folder (`file_stem()` — `"Foo.renderly"` — plus `-assets`). See
//! [`assets_dir_for_project`]. Content-keying within that directory is unchanged
//! (`cache_key`, a hash of the media path + mtime), so a media file renamed or re-imported
//! into the same project still hits.
//!
//! Callers resolve the directory once per operation via [`resolve_cache_dir`], which falls
//! back to the old global `$APPCACHE/media-cache` dir when there is no project path yet
//! (there always is one in this codebase — every session-creating command writes its
//! project file to disk immediately — but this keeps the module correct even if that
//! invariant ever changes). [`generate_assets_blocking`] additionally migrates a hit from
//! that old global cache into the sidecar directory the first time it's asked to look at a
//! project-local dir that doesn't have the entry yet, so upgrading doesn't force a
//! regeneration wave.

use renderly_core::project::MediaKind;
use renderly_core::{audio_peaks, generate_thumbnail_strip};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tauri::{AppHandle, Emitter, Manager};

const MAX_TILES: u32 = 24;
const TILE_HEIGHT: u32 = 72;
const WAVEFORM_BUCKETS: u32 = 512;
/// Bumped when generation params change so stale heavy strips/peaks are rebuilt once.
const CACHE_VERSION: &str = "v2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThumbInfo {
    pub cols: u32,
    pub rows: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub interval_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CachedAssets {
    /// Filename (not full path — the cache dir can move between app data locations)
    /// of the strip PNG, alongside this metadata file. `None` for audio-only media.
    strip_file: Option<String>,
    thumb: Option<ThumbInfo>,
    peaks: Vec<f32>,
    bucket_secs: f64,
}

/// Emitted on `media:thumbnails-ready`. `strip_path` is a real filesystem path — the
/// frontend converts it to a loadable URL via `convertFileSrc` (the Tauri asset protocol
/// is scoped to the media cache dir; see tauri.conf.json).
#[derive(Debug, Clone, Serialize)]
struct ThumbnailsReadyEvent {
    media_id: String,
    strip_path: String,
    cols: u32,
    rows: u32,
    tile_width: u32,
    tile_height: u32,
    interval_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
struct WaveformReadyEvent {
    media_id: String,
    peaks: Vec<f32>,
    bucket_secs: f64,
}

/// Returned by the `get_media_assets` command — whatever's already cached, synchronously,
/// with no generation triggered. Either field may be absent if that half hasn't been
/// generated yet (or never will be, e.g. `thumbnails` for audio-only media).
#[derive(Debug, Clone, Serialize, Default)]
pub struct MediaAssetsPayload {
    thumbnails: Option<ThumbnailsReadyEvent>,
    waveform: Option<WaveformReadyEvent>,
}

/// The old global cache dir — still used as a fallback when no project path is available
/// yet, and as a migration source (see module doc comment).
fn global_cache_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("media-cache");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Sidecar assets directory for a project file: `Foo.renderly.json` → `Foo.renderly-assets/`
/// next to it. See the module doc comment for the full rationale.
pub fn assets_dir_for_project(project_path: &Path) -> PathBuf {
    let stem = project_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let mut dir = project_path.to_path_buf();
    dir.set_file_name(format!("{stem}-assets"));
    dir
}

/// Resolve (and create) the cache directory generation/lookup should use for the current
/// session: the project-local sidecar dir when a project path is known, otherwise the old
/// global app-cache dir.
pub fn resolve_cache_dir(app: &AppHandle, project_path: Option<&Path>) -> Result<PathBuf, String> {
    match project_path {
        Some(p) => {
            let dir = assets_dir_for_project(p);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            Ok(dir)
        }
        None => global_cache_dir(app),
    }
}

/// If `dir` (a resolved project-local sidecar) is missing the cache entry for `key` but the
/// old global cache dir has it, copy it over so it isn't regenerated. No-op (and cheap) once
/// migrated, since the caller only calls this on a cache miss in `dir`.
fn migrate_from_global_cache(app: &AppHandle, dir: &Path, key: &str) {
    let Ok(old_dir) = global_cache_dir(app) else {
        return;
    };
    if old_dir == dir {
        return;
    }
    let old_meta = old_dir.join(format!("{key}.json"));
    if !old_meta.is_file() {
        return;
    }
    let new_meta = dir.join(format!("{key}.json"));
    if std::fs::copy(&old_meta, &new_meta).is_err() {
        return;
    }
    let old_strip = old_dir.join(format!("{key}.png"));
    if old_strip.is_file() {
        let _ = std::fs::copy(&old_strip, dir.join(format!("{key}.png")));
    }
}

/// Not a cryptographic hash — this only needs to be a stable, collision-unlikely cache
/// key for one machine's local disk cache, not a security boundary. Avoids adding a
/// sha1/sha2 dependency for what `std::hash::Hash` already does well enough here.
fn cache_key(path: &Path) -> String {
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    mtime.hash(&mut hasher);
    format!("{CACHE_VERSION}-{:016x}", hasher.finish())
}

fn read_cached(meta_path: &Path) -> Option<CachedAssets> {
    let data = std::fs::read_to_string(meta_path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cached(meta_path: &Path, cached: &CachedAssets) -> Result<(), String> {
    let data = serde_json::to_string(cached).map_err(|e| e.to_string())?;
    std::fs::write(meta_path, data).map_err(|e| e.to_string())
}

fn to_payload(dir: &Path, cached: &CachedAssets, media_id: &str) -> MediaAssetsPayload {
    let thumbnails = match (&cached.strip_file, &cached.thumb) {
        (Some(file), Some(thumb)) => Some(ThumbnailsReadyEvent {
            media_id: media_id.to_string(),
            strip_path: dir.join(file).to_string_lossy().to_string(),
            cols: thumb.cols,
            rows: thumb.rows,
            tile_width: thumb.tile_width,
            tile_height: thumb.tile_height,
            interval_secs: thumb.interval_secs,
        }),
        _ => None,
    };
    let waveform = if cached.peaks.is_empty() {
        None
    } else {
        Some(WaveformReadyEvent {
            media_id: media_id.to_string(),
            peaks: cached.peaks.clone(),
            bucket_secs: cached.bucket_secs,
        })
    };
    MediaAssetsPayload {
        thumbnails,
        waveform,
    }
}

fn emit_thumbnails(app: &AppHandle, media_id: &str, dir: &Path, cached: &CachedAssets) {
    if let Some(thumbnails) = to_payload(dir, cached, media_id).thumbnails {
        let _ = app.emit("media:thumbnails-ready", thumbnails);
    }
}

fn emit_waveform(app: &AppHandle, media_id: &str, dir: &Path, cached: &CachedAssets) {
    if let Some(waveform) = to_payload(dir, cached, media_id).waveform {
        let _ = app.emit("media:waveform-ready", waveform);
    }
}

fn emit_ready(app: &AppHandle, media_id: &str, dir: &Path, cached: &CachedAssets) {
    emit_thumbnails(app, media_id, dir, cached);
    emit_waveform(app, media_id, dir, cached);
}

fn generate_assets_blocking(
    app: &AppHandle,
    media_id: &str,
    path: &Path,
    kind: MediaKind,
    dir: &Path,
) -> Result<(), String> {
    let key = cache_key(path);
    let meta_path = dir.join(format!("{key}.json"));

    if read_cached(&meta_path).is_none() {
        migrate_from_global_cache(app, dir, &key);
    }
    if let Some(cached) = read_cached(&meta_path) {
        emit_ready(app, media_id, dir, &cached);
        return Ok(());
    }

    // Waveform runs in parallel with the filmstrip so wall-clock time is closer to
    // max(thumbs, peaks) than sum. Thumbnails emit as soon as the strip is ready so
    // the timeline paints before peaks finish.
    let need_wave = kind != MediaKind::Image;
    let wave_path = path.to_path_buf();
    let wave_handle = need_wave.then(|| {
        std::thread::spawn(move || match audio_peaks(&wave_path, WAVEFORM_BUCKETS) {
            Ok(p) if !p.peaks.is_empty() => (p.peaks, p.bucket_secs),
            Ok(_) => (Vec::new(), 0.0),
            Err(e) => {
                eprintln!("media assets: waveform generation failed: {e}");
                (Vec::new(), 0.0)
            }
        })
    });

    let mut cached = CachedAssets::default();
    if kind == MediaKind::Video {
        let strip_path = dir.join(format!("{key}.png"));
        match generate_thumbnail_strip(path, &strip_path, MAX_TILES, TILE_HEIGHT) {
            Ok(strip) => {
                cached.strip_file = Some(format!("{key}.png"));
                cached.thumb = Some(ThumbInfo {
                    cols: strip.cols,
                    rows: strip.rows,
                    tile_width: strip.tile_width,
                    tile_height: strip.tile_height,
                    interval_secs: strip.interval_secs,
                });
                let _ = write_cached(&meta_path, &cached);
                emit_thumbnails(app, media_id, dir, &cached);
            }
            Err(e) => eprintln!("media assets: thumbnail generation failed for {media_id}: {e}"),
        }
    }

    if let Some(handle) = wave_handle {
        let (peaks, bucket_secs) = handle.join().unwrap_or_else(|_| (Vec::new(), 0.0));
        cached.peaks = peaks;
        cached.bucket_secs = bucket_secs;
        write_cached(&meta_path, &cached)?;
        emit_waveform(app, media_id, dir, &cached);
    } else {
        write_cached(&meta_path, &cached)?;
    }

    Ok(())
}

/// Kick off thumbnail/waveform generation for one media item in the background. Safe to
/// call redundantly (e.g. once per media item on every project open) — a cache hit is a
/// cheap file read, not a re-run of ffmpeg. `dir` is the already-resolved cache directory
/// for the current project — see [`resolve_cache_dir`].
pub fn request_assets(
    app: AppHandle,
    media_id: String,
    path: PathBuf,
    kind: MediaKind,
    dir: PathBuf,
) {
    // A nested sequence has no file to thumbnail or waveform (schema v7).
    if kind == MediaKind::Sequence {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let media_id_for_log = media_id.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            generate_assets_blocking(&app, &media_id, &path, kind, &dir)
        })
        .await;
        if let Ok(Err(e)) = result {
            eprintln!("media assets: generation failed for {media_id_for_log}: {e}");
        }
    });
}

/// Whatever's already cached for `path`, synchronously, with no generation triggered —
/// for the frontend to check on-demand (e.g. a media item added to the bin mid-session by
/// something other than the normal import flow, or as a fallback pull after a project
/// reopen in case the push event raced the frontend's listener setup). `dir` is the
/// already-resolved cache directory for the current project — see [`resolve_cache_dir`].
/// Also attempts the old-global-cache migration on a miss, same as generation, so a pull
/// alone (no generation needed) still upgrades an entry into the sidecar dir.
pub fn get_cached(
    app: &AppHandle,
    media_id: &str,
    path: &Path,
    dir: &Path,
) -> Result<MediaAssetsPayload, String> {
    let key = cache_key(path);
    let meta_path = dir.join(format!("{key}.json"));
    if read_cached(&meta_path).is_none() {
        migrate_from_global_cache(app, dir, &key);
    }
    Ok(match read_cached(&meta_path) {
        Some(cached) => to_payload(dir, &cached, media_id),
        None => MediaAssetsPayload::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_dir_next_to_dotted_project_filename() {
        let project = Path::new("/home/user/Projects/Foo.renderly.json");
        let dir = assets_dir_for_project(project);
        assert_eq!(dir, Path::new("/home/user/Projects/Foo.renderly-assets"));
    }

    #[test]
    fn sidecar_dir_preserves_parent_directory() {
        let project = Path::new("C:/Users/me/Videos/My Edit.renderly.json");
        let dir = assets_dir_for_project(project);
        assert_eq!(dir, Path::new("C:/Users/me/Videos/My Edit.renderly-assets"));
    }

    #[test]
    fn sidecar_dir_handles_extensionless_filename() {
        let project = Path::new("/tmp/Untitled");
        let dir = assets_dir_for_project(project);
        assert_eq!(dir, Path::new("/tmp/Untitled-assets"));
    }
}
