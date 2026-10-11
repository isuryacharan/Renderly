//! Files the webview preview compositor needs but cannot read itself (docs/preview-webview.md
//! P4b): raster / generated matte images and pack `.cube` LUTs.
//!
//! The wasm compositor has no filesystem, and unlike media files these live at arbitrary
//! paths (a mask's PNG anywhere on disk, a baked `matte.png` in a cache dir, a pack folder),
//! so rather than widening the asset-protocol scope per file we expose three small read
//! commands. Each is **restricted to what the open project actually references** (derived
//! with the same `renderly-core` walkers the compositor uses to ask for them), so the
//! webview cannot use them as a general file reader.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use renderly_core::mask::project_matte_requests;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct PackLutRef {
    pub pack_id: String,
    pub lut_id: String,
}

#[derive(Debug, Serialize)]
pub struct PreviewPackLut {
    pub pack_id: String,
    pub lut_id: String,
    /// Raw `.cube` text; parsed by `renderly_core::packs::parse_cube_text` in the wasm crate.
    pub cube: String,
}

/// Paths of every matte file the open project references.
fn referenced_matte_paths(state: &AppState) -> Result<Vec<PathBuf>, String> {
    state.with_session(|s| {
        Ok(project_matte_requests(&s.project)
            .into_iter()
            .map(|r| r.path)
            .collect())
    })
}

/// Raw bytes of an encoded matte image (PNG/JPEG/...) referenced by the open project. The
/// webview decodes it (`createImageBitmap`) and hands RGBA to the wasm compositor.
#[tauri::command]
pub async fn read_preview_matte(
    state: State<'_, AppState>,
    path: String,
) -> Result<tauri::ipc::Response, String> {
    let wanted = PathBuf::from(&path);
    if !referenced_matte_paths(&state)?.contains(&wanted) {
        return Err(format!("not a matte of the open project: {path}"));
    }
    let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(&wanted))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("read matte {path}: {e}"))?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Modification time (ms since epoch; 0 when unreadable) of each requested matte file, so the
/// webview can notice a matte regenerated at the same path (`GenerateBackgroundMatte` rewrites
/// `matte.png` in place and the project JSON does not change). Unreferenced paths report 0.
#[tauri::command]
pub async fn preview_matte_stamps(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<Vec<u64>, String> {
    let referenced = referenced_matte_paths(&state)?;
    Ok(paths
        .iter()
        .map(|p| {
            let path = PathBuf::from(p);
            if !referenced.contains(&path) {
                return 0;
            }
            std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64)
        })
        .collect())
}

/// `.cube` text for each requested `(pack_id, lut_id)` found in the open project's loaded
/// packs (`asset_pack_paths`). Pairs that cannot be resolved are simply omitted - export skips
/// such effects too.
#[tauri::command]
pub async fn read_preview_pack_luts(
    state: State<'_, AppState>,
    wanted: Vec<PackLutRef>,
) -> Result<Vec<PreviewPackLut>, String> {
    let pack_roots = state.with_session(|s| Ok(s.project.asset_pack_paths.clone()))?;
    tauri::async_runtime::spawn_blocking(move || resolve_pack_luts(&pack_roots, &wanted))
        .await
        .map_err(|e| e.to_string())
}

fn resolve_pack_luts(roots: &[PathBuf], wanted: &[PackLutRef]) -> Vec<PreviewPackLut> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(text) = std::fs::read_to_string(root.join("pack.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<renderly_core::packs::PackManifest>(&text) else {
            continue;
        };
        for want in wanted.iter().filter(|w| w.pack_id == manifest.id) {
            let Some(lut) = manifest.luts.iter().find(|l| l.id == want.lut_id) else {
                continue;
            };
            if let Ok(cube) = std::fs::read_to_string(root.join(&lut.path)) {
                out.push(PreviewPackLut {
                    pack_id: want.pack_id.clone(),
                    lut_id: want.lut_id.clone(),
                    cube,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_only_requested_luts_from_matching_pack() {
        let dir = std::env::temp_dir().join(format!("renderly-pa-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.json"),
            r#"{"id":"p","name":"P","luts":[{"id":"a","label":"A","path":"a.cube"},{"id":"b","label":"B","path":"b.cube"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("a.cube"), "cube-a").unwrap();
        std::fs::write(dir.join("b.cube"), "cube-b").unwrap();
        let got = resolve_pack_luts(
            std::slice::from_ref(&dir),
            &[
                PackLutRef {
                    pack_id: "p".into(),
                    lut_id: "b".into(),
                },
                PackLutRef {
                    pack_id: "other".into(),
                    lut_id: "a".into(),
                },
                PackLutRef {
                    pack_id: "p".into(),
                    lut_id: "missing".into(),
                },
            ],
        );
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].lut_id.as_str(), got[0].cube.as_str()),
            ("b", "cube-b")
        );
    }
}
