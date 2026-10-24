//! Files the webview preview compositor needs but cannot read itself (docs/preview-webview.md
//! P4b): raster / generated matte images, and the pack items (LUTs, effects, filters,
//! transitions) the project references by ref.
//!
//! The wasm compositor has no filesystem, and unlike media files these live at arbitrary
//! paths (a mask's PNG anywhere on disk, a baked `matte.png` in a cache dir, a pack folder),
//! so rather than widening the asset-protocol scope per file we expose three small read
//! commands. Each is **restricted to what the open project actually references** (derived
//! with the same `renderly-core` walkers the compositor uses to ask for them), so the
//! webview cannot use them as a general file reader.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use base64::Engine as _;
use renderly_core::packs::{host::host_item, referenced_pack_items, LoadedPack};

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
    // The single matte image a mask / baked background removal names, or one frame of a
    // per-frame matte sequence (`f000123.png` in a baked cache dir of the open project).
    let allowed = referenced_matte_paths(&state)?.contains(&wanted)
        || state.with_session(|s| {
            Ok(renderly_core::mask::is_frame_matte_path(
                &s.project, &wanted,
            ))
        })?;
    if !allowed {
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

/// `.cube` text for each requested `(pack_id, lut_id)` found in the installed packs. Pairs that
/// cannot be resolved are simply omitted - export skips such effects too. LUT-only entry point
/// kept for older callers; `read_preview_pack_items` serves every item kind by ref.
#[tauri::command]
pub async fn read_preview_pack_luts(
    wanted: Vec<PackLutRef>,
) -> Result<Vec<PreviewPackLut>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        resolve_pack_luts(&renderly_core::packs::load_installed_packs(), &wanted)
    })
    .await
    .map_err(|e| e.to_string())
}

fn resolve_pack_luts(packs: &[LoadedPack], wanted: &[PackLutRef]) -> Vec<PreviewPackLut> {
    let mut out = Vec::new();
    for want in wanted {
        let Some(pack) = renderly_core::packs::find_pack(packs, &want.pack_id) else {
            continue;
        };
        let Some(lut) = pack.manifest.luts.iter().find(|l| l.id == want.lut_id) else {
            continue;
        };
        if let Ok(cube) = std::fs::read_to_string(pack.root.join(&lut.path)) {
            out.push(PreviewPackLut {
                pack_id: want.pack_id.clone(),
                lut_id: want.lut_id.clone(),
                cube,
            });
        }
    }
    out
}

/// One `pack:` ref the webview wants the files of (an entry of the wasm compositor's
/// `required_assets().pack_items`).
#[derive(Debug, Deserialize)]
pub struct PackItemWanted {
    #[serde(rename = "ref")]
    pub item_ref: String,
    #[serde(default)]
    pub hash: Option<String>,
}

/// An encoded texture file of an effect; the webview decodes it (`createImageBitmap`) and
/// passes RGBA to the wasm compositor's `set_pack_item`.
#[derive(Debug, Serialize)]
pub struct PreviewTexture {
    pub path: String,
    pub bytes_base64: String,
}

/// The files of one pack item, shaped for `WasmCompositor.set_pack_item(item_json, textures)`:
/// everything except `textures` is its `item_json`.
#[derive(Debug, Serialize)]
pub struct PreviewPackItem {
    #[serde(rename = "ref")]
    pub item_ref: String,
    pub pack_id: String,
    pub item_id: String,
    pub version: u32,
    /// `lut`, `effect`, `filter` or `transition`.
    pub kind: String,
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cube: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    pub shaders: BTreeMap<String, String>,
    pub textures: Vec<PreviewTexture>,
}

/// The files of each requested pack item the open project references and the library can run.
/// Refs the project does not use (unless `include_installed`, which hover preview sets),
/// builtin refs (compiled into the wasm build) and items that are missing or cannot run are
/// omitted: export renders those pass-through too.
#[tauri::command]
pub async fn read_preview_pack_items(
    state: State<'_, AppState>,
    wanted: Vec<PackItemWanted>,
    include_installed: Option<bool>,
) -> Result<Vec<PreviewPackItem>, String> {
    // Hover preview asks for installed items the project does not use yet. They still resolve
    // by ref against the user's library only, so this is not a general file reader.
    let allow_all = include_installed.unwrap_or(false);
    let referenced: Vec<String> = if allow_all {
        wanted.iter().map(|w| w.item_ref.clone()).collect()
    } else {
        state.with_session(|s| {
            Ok(referenced_pack_items(&s.project)
                .into_iter()
                .map(|u| u.item_ref)
                .collect())
        })?
    };
    tauri::async_runtime::spawn_blocking(move || {
        let packs = renderly_core::packs::load_installed_packs();
        resolve_pack_items(&packs, &referenced, &wanted)
    })
    .await
    .map_err(|e| e.to_string())
}

fn resolve_pack_items(
    packs: &[LoadedPack],
    referenced: &[String],
    wanted: &[PackItemWanted],
) -> Vec<PreviewPackItem> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    for want in wanted {
        if !referenced.contains(&want.item_ref) {
            continue;
        }
        let Ok(item) = host_item(packs, &want.item_ref, want.hash.as_deref()) else {
            continue;
        };
        out.push(PreviewPackItem {
            item_ref: want.item_ref.clone(),
            pack_id: item.pack_id,
            item_id: item.item_id,
            version: item.version,
            kind: item.kind,
            hash: item.hash,
            cube: item.cube,
            manifest: item.manifest,
            shaders: item.shaders,
            textures: item
                .textures
                .into_iter()
                .map(|t| PreviewTexture {
                    path: t.path,
                    bytes_base64: engine.encode(t.bytes),
                })
                .collect(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pack() -> (PathBuf, LoadedPack) {
        let dir = std::env::temp_dir().join(format!("renderly-pa-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.json"),
            r#"{"id":"p","name":"P","luts":[{"id":"a","label":"A","path":"a.cube"},{"id":"b","label":"B","path":"b.cube"}]}"#,
        )
        .unwrap();
        let cube = |n: u32| {
            let mut t = String::from("LUT_3D_SIZE 2\n");
            for b in 0..2 {
                for g in 0..2 {
                    for r in 0..2 {
                        t.push_str(&format!(
                            "{} {} {}\n",
                            r as f32,
                            g as f32,
                            (b + n) as f32 * 0.0 + b as f32
                        ));
                    }
                }
            }
            t
        };
        std::fs::write(dir.join("a.cube"), cube(0)).unwrap();
        std::fs::write(dir.join("b.cube"), cube(1)).unwrap();
        let pack = renderly_core::packs::load_pack(&dir).unwrap();
        (dir, pack)
    }

    #[test]
    fn resolves_only_requested_luts_from_matching_pack() {
        let (dir, pack) = temp_pack();
        let got = resolve_pack_luts(
            std::slice::from_ref(&pack),
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
        assert_eq!(got[0].lut_id, "b");
        assert!(got[0].cube.starts_with("LUT_3D_SIZE 2"));
    }

    #[test]
    fn pack_items_are_served_by_ref_and_only_when_the_project_uses_them() {
        let (dir, pack) = temp_pack();
        let packs = std::slice::from_ref(&pack);
        let wanted = |r: &str| {
            vec![PackItemWanted {
                item_ref: r.to_string(),
                hash: None,
            }]
        };
        let used = vec!["pack:local.p/a@1".to_string()];
        // A referenced LUT comes back with its .cube text (format-1 id `p` is `local.p`).
        let got = resolve_pack_items(packs, &used, &wanted("pack:local.p/a@1"));
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].kind.as_str(), got[0].pack_id.as_str()),
            ("lut", "local.p")
        );
        assert!(got[0].cube.is_some() && got[0].manifest.is_none());
        // Not referenced by the project: refused. Builtins and unknown refs: omitted.
        assert!(resolve_pack_items(packs, &used, &wanted("pack:local.p/b@1")).is_empty());
        assert!(resolve_pack_items(packs, &used, &wanted("builtin:blur@1")).is_empty());
        let all = vec!["pack:local.p/zzz@1".to_string()];
        assert!(resolve_pack_items(packs, &all, &wanted("pack:local.p/zzz@1")).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
