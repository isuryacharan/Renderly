# Asset packs (Phase 3)

Declarative, no-code extensions. A pack is a directory containing `pack.json` plus assets.

## Layout

```
my-pack/
  pack.json
  luts/
    film.cube
  stickers/
    star.png
  sfx/
    blip.wav
```

## `pack.json` (v1)

```jsonc
{
  "id": "my-pack",          // unique slug; used in effect ids
  "name": "My Pack",
  "version": "1",
  "caption_styles": [ /* optional */ ],
  "luts": [
    { "id": "film", "label": "Film", "path": "luts/film.cube" }
  ],
  "transitions": [
    // Aliases of builtin TransitionKind only — no custom shaders in v1
    { "id": "quick", "label": "Quick wipe", "kind": "wipe_left", "default_duration_secs": 0.35 }
  ],
  "stickers": [
    { "id": "star", "label": "Star", "path": "stickers/star.png", "default_duration_secs": 3.0 }
  ],
  "sfx": [
    { "id": "blip", "label": "Blip", "path": "sfx/blip.wav" }
  ],
  "templates": [
    {
      "id": "cold-open",
      "label": "Cold open",
      "commands": [
        // Serialized Command values; AddClip.position_secs is offset by ApplyTemplate
      ]
    }
  ]
}
```

### Caption styles

Fields: `id`, `label`, `font_scale` (relative), `fill_rgba`, optional `stroke_rgba` /
`shadow_rgba` / `box_rgba`, `anchor` (`top`|`center`|`bottom`). Resolved after builtins
when burning captions.

### LUTs

IRIDAS `.cube` (`LUT_3D_SIZE`). Applied on the CPU before GPU upload. Effect id:

`pack:<pack_id>:lut:<lut_id>`

Params: `intensity` (0..1, default 1).

### Stickers

PNG (or other image) placed as an image `MediaClip` on a video track (no new clip type).
Command: `AddStickerFromPack { pack_id, sticker_id, track_id, position_secs }` — imports
the asset into the media pool if needed, then `AddClip`.

### SFX

Audio file placed as an audio `MediaClip`. Command: `AddSfxFromPack { pack_id, sfx_id,
track_id, position_secs }`.

### Templates (Phase 4)

Named command sequences. Each entry has `id`, `label`, and `commands` (JSON `Command`
objects). `ApplyTemplate { pack_id, template_id, position_secs }` deserializes each
command, adds `position_secs` to every `AddClip.position_secs`, and applies them
sequentially — rolling back the project clone on any failure.

## Pack format 2 (effects, filters, transitions, LUTs)

Set `"format": 2`. The format-1 fields above still work in format 1 packs; a format-2 pack
declares typed `items` instead. Full design: [effect-pack-format.md](effect-pack-format.md).

```jsonc
{
  "format": 2,
  "id": "mira.film-looks",          // <publisher>.<name>, each part [a-z0-9-]
  "name": "Film Looks",
  "release": "1.3.0",               // semver of this pack release (required)
  "license": "CC-BY-4.0",           // SPDX id or "user-supplied" (required in format 2)
  "authors": ["Mira K."],
  "description": "Grain and print looks.",
  "tags": ["film"],
  "requires": { "renderly": ">=0.9" }, // optional; unmet => every item is disabled
  "items": [
    { "kind": "effect", "path": "effects/film-grain/1" },
    { "kind": "effect", "path": "effects/film-grain/2" },
    { "kind": "transition", "path": "transitions/whip/1" },
    { "kind": "lut", "id": "cool", "label": "Cool", "path": "luts/cool.cube" }
  ]
}
```

- **`effect`, `filter`, `transition`** point at a folder holding `effect.json` and its WGSL
  (and textures). The item's id, version and label come from `effect.json`. Several versions
  of one id can sit side by side (`film-grain@1` and `@2`); a second copy of the same
  `id@version` in one pack is an error for that copy.
- **`lut`** is inline: `id` (`[a-z0-9_-]`), `label`, `path`, optional `version` (default 1) and
  `layout`. Only `.cube` loads for now. PNG layouts (`grid`, `strip`, `hald`) are reported as
  not supported yet.
- **Unknown kinds** are skipped with `needs a newer Renderly (item kind `x`)`; the rest of the
  pack loads. The format-1 kinds (`sticker`, `sfx`, `caption_style`, `template`) are not loaded
  from format-2 packs yet and are skipped.
- **Refused packs**: an unknown `format`, a bad `id`, `release` or `license`, or a `requires`
  that does not parse refuses the whole pack with an error. A `requires.renderly` this build
  does not meet installs the pack with every item disabled.
- Format-1 packs (no `format`) load as before and are also seen as items. Their ids answer to
  `local.<id>` in refs as well (`starter` is `local.starter`).

### Features

Items declare the engine features they need in `requires` (default `["gpu.fragment",
"expr.v1"]`). This build supports exactly `gpu.fragment` and `expr.v1`
(`renderly_core::packs::FEATURES`). An item that needs anything else is installed but
**disabled** with ``needs a newer Renderly (feature `gpu.compute`)``. It never renders half-right.

### Item states

| state | meaning |
|---|---|
| `ok` | loaded, validated, usable |
| `disabled` | installed, but this build cannot run it (a feature it lacks, or the pack's `requires`) |
| `error` | failed to load: broken WGSL, missing file, bad manifest, bad LUT, or a duplicate `id@version`. The rest of the pack still loads |
| `skipped` | its kind is unknown to this build |

Load-time validation for effects is the runtime's: the manifest and its expressions are parsed
and type-checked, every pass is validated by naga with its prelude, and the format limits are
enforced. The same checks run for builtins.

### Refs and hashes

- Refs: `pack:<pack>/<item>@<version>` for pack items, `builtin:<id>@<version>` for builtins.
  `pack` may be the pack id or, for format 1, `local.<id>`.
- Resolution against loaded packs gives **ok**, **changed** (an expected hash differs from the
  installed one; the installed files render and the caller marks the instance), **missing**
  (not installed; the caller renders pass-through and keeps the params) or **disabled** (with
  the reason).
- An item's **hash** is SHA-256 (lowercase hex) over its `effect.json` (or, for a LUT, its
  declaration) and every file it references, named relative to the item folder. Per-file
  digests are cached by `(path, mtime, size)`, so reloading an unchanged pack does not rehash
  it. A rewrite that keeps both mtime and size is not seen until the process restarts.
- Project refs to pack items are stored by schema v8. Today `SetClipEffects` still accepts
  `builtin:<id>` and `pack:<pack>:lut:<lut>`.

### Checking a pack

```
renderly pack validate <dir>          # text: pack summary, one line per item, hashes
renderly pack validate <dir> --json   # the same report as JSON
```

Exits non-zero if the pack is refused or any item is in the `error` state.

### Perception

`describe_effects` (MCP) lists the builtins plus the packs you name by folder, or the open
project's packs when you name none. `describe_commands` lists the same catalogue. Each line
gives the ref, kind, label, state (with the reason when not `ok`) and the params: type, range,
default and `initial` (the value written when the effect is added).

## Commands

- `LoadAssetPack { path }` — validate + record path on the project
- `UnloadAssetPack { pack_id }`
- `AddStickerFromPack` / `AddSfxFromPack`
- `ApplyTemplate`

See example: [`examples/packs/starter`](../examples/packs/starter).
Registry seed: [`examples/registry/index.json`](../examples/registry/index.json).
