# Project schema — v6

Status: **current**. This is the source of truth for `renderly-core`'s project
model. Implementation types in `renderly-core/src/project/` must match this document; if
they diverge, fix whichever one is wrong and note it in the same PR. Schema changes bump
`schema_version` and are documented in the "Version history" section at the bottom.

The project file is a single JSON document, human-readable, git-diffable, on disk with
extension `.renderly.json`.

## Top-level shape

```jsonc
{
  "schema_version": 6,
  "id": "b3f1c2a0-...-uuid",
  "name": "ultra-bruno-ep12",
  "settings": { "fps": 60.0, "width": 1080, "height": 1920, "sample_rate": 48000, "duck_db": -12.0 },
  "media": [ /* MediaItem[] */ ],
  "tracks": [ /* Track[] */ ],
  "asset_pack_paths": [],
  "wasm_plugin_paths": [],
  "multicam_groups": [],
  "segmentation_model_path": null
}
```

| Field | Type | Notes |
|---|---|---|
| `schema_version` | u32 | `6` for this spec. Loaders accept `1`..=`6`; saves write `6`. |
| `multicam_groups` | MulticamGroup[] | Optional sync groups (Phase 4). |
| `segmentation_model_path` | path? | Optional local model marker for BG removal. |

## MediaClip (Phase 4 fields)

| Field | Notes |
|---|---|
| `mask` | Optional `ClipMask` — shape / raster / generated matte; invert + feather.
  Shape UVs are source-frame (0..1). Rect/ellipse authoring is via the app Mask tool
  overlay + `SetClipMask` (live drag patches the Zustand store directly for the preview
  canvas's own redraw — no backend round trip; see docs/preview-webview.md P4). |
| `background_removal` | Optional config (`model_id` `heuristic` or CLI-backed `rvm`/`birefnet`, threshold, feather, `matte_cache_dir`). |
| `audio_denoise` | Optional `{ enabled, backend, strength }` — **audio tracks only** in v1. `backend` is `"afftdn"` (FFmpeg filter; the default when the field is absent) or `"rnnoise"` (RNNoise via `nnnoiseless`; `strength` = wet/dry mix). Added without a schema bump. |
| `multicam_group_id` | Optional link into `project.multicam_groups`. |

### Builtin effects (additions)

| `effect_id` | Params |
|---|---|
| `builtin:chroma_key` | `key_r`, `key_g`, `key_b` (key colour, 0..1, default green), `tolerance` (0..1, default 0.3: RGB distance at or below which a pixel is fully transparent), `softness` (0..1, default 0.1: width of the linear alpha ramp above `tolerance`), `spill` (optional, 0..1, default 0 = off: strips the key colour's chroma from RGB; absent == 0 so older projects render unchanged) |

### Effect / matte order

source decode → pack LUT → WASM frame → **chroma key (GPU WGSL effect pass, in effect-chain order)** → **background-removal matte** → **clip mask** → GPU builtins (color/blur/lut/glitch) → composite blend.

## Version history

- **v5**: Speed keyframes; stickers/SFX; audio WASM.
- **v6** (Phase 4): `ClipMask`, background removal, audio denoise, multicam groups; chroma key builtin.
