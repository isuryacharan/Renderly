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
    { "id": "quick", "label": "Quick wipe", "kind": "wipe_left", "default_duration_us": 350000 }
  ],
  "stickers": [
    { "id": "star", "label": "Star", "path": "stickers/star.png", "default_duration_us": 3000000 }
  ],
  "sfx": [
    { "id": "blip", "label": "Blip", "path": "sfx/blip.wav" }
  ],
  "templates": [
    {
      "id": "cold-open",
      "label": "Cold open",
      "commands": [
        // Serialized Command values; AddClip.start is offset by ApplyTemplate
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

IRIDAS `.cube` (`LUT_3D_SIZE`). Uploaded as a 3D texture and applied on the GPU by the builtin
LUT filter (`builtin:lut_filter@1`). Ref (schema v8):

`pack:<pack-id>/<lut-id>@1` (a format-1 pack answers to `local.<id>`, so `pack:local.my-pack/film@1`)

Params: `intensity` (0..1, default 1). Apply it with `SetClipEffects`, usually in the `filter`
slot. The v7 effect id `pack:<pack_id>:lut:<lut_id>` is gone from project files; the migration
rewrote it to the ref above.

### Stickers

PNG (or other image) placed as an image `MediaClip` on a video track (no new clip type).
Command: `AddStickerFromPack { pack_id, sticker_id, track_id, start }` — imports
the asset into the media pool if needed, then `AddClip`.

### SFX

Audio file placed as an audio `MediaClip`. Command: `AddSfxFromPack { pack_id, sfx_id,
track_id, start }`.

### Templates (Phase 4)

Named command sequences. Each entry has `id`, `label`, and `commands` (JSON `Command`
objects). `ApplyTemplate { pack_id, template_id, start }` deserializes each
command, adds `start` to every `AddClip.start`, and applies them
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
- Projects store these refs (and the hash an item had when it was applied) since schema v8;
  they never store pack paths. A format-1 pack's transition alias (`pack:local.starter/quick-wipe@1`)
  runs the builtin it names. `pack:<pack>/<lut>@<v>` resolves to `builtin:lut_filter@1` with that
  LUT bound.
- Status (`ok` / `changed` / `missing` / `disabled`) is computed on load from what is installed,
  never stored, and a project's problem items are listed by `get_project` (MCP) and marked in the
  in-app outline. `missing` renders pass-through and keeps the instance's params. See
  [project-schema.md](project-schema.md) "Effects, slots and blend modes".
- Slots: `kind: "filter"` items (and LUTs) belong in a clip's `filter` slot, a keying effect
  (`category: "key"`) in `key`, the colour adjustment in `adjust`, everything else in `effects[]`;
  slots always run in the order filter, key, adjust, effects[]. A parameter marked
  `animatable: false`, and `bool`, `choice` and `lut` parameters, cannot be keyframed.

### Checking a pack

```
renderly pack validate <dir>          # text: pack summary, one line per item, hashes
renderly pack validate <dir> --json   # the same report as JSON
```

Exits non-zero if the pack is refused or any item is in the `error` state.

### The library, `.rpack` and installing (what works now)

Packs are installed into a per-user library, not referenced by project paths.
Library location: `RENDERLY_LIBRARY_DIR` if set, else `<user data dir>/Renderly/library`
(`%APPDATA%` on Windows, `~/Library/Application Support` on macOS, `$XDG_DATA_HOME` or
`~/.local/share` elsewhere). Every command takes `--library <dir>` to use another folder.

```
renderly pack build <dir> [-o <file.rpack | folder>] [--json]  # reproducible .rpack
renderly pack install <file.rpack | folder> [--json]           # validate, then copy in
renderly pack uninstall <pack-id> [--release <R>] [--json]     # copies deleted
renderly pack list [--json]                                    # releases, items, states
renderly pack link <dir> [--json]                              # dev folder, read on every load
```

- A `.rpack` is a ZIP with `pack.json` at its root. `pack build` writes entries sorted, with
  fixed timestamps, permissions and compression, so the same source gives the same bytes and
  SHA-256. The default name is `<pack-id>-<release>.rpack`. The build refuses a pack that does
  not load or has an item in the `error` state.
- Archives are checked before extraction: at most 10,000 entries and 1 GiB uncompressed, no
  entry over 100:1, no absolute paths, `..` or backslashes, no symbolic links, and a
  `pack.json` at the top level (a wrapping folder is refused). Pack folders are held to the
  same limits when copied.
- Install runs the same loader as `pack validate`. A pack-level error refuses the install. An
  item-level error installs the pack and reports that item, with its reason.
- Refused: the same `id` and `release` with different content (`a different file claims to be
  <name> <release>`), and an item version already installed from another release with a
  different hash (published versions never change; make a new version). Installing the same
  release with the same content again is a no-op (`already installed`).
- Several releases of one pack can be installed side by side. A ref resolves to the release
  whose hash matches, and when hashes do not decide, the newest release (semver compare).
- `library.json` records each release: `pack_id`, `release`, `format`, `license`,
  `archive_sha256` (file installs), `content_sha256`, `source` (`file`, `folder`,
  `dev:<path>`, `builtin`), `installed_at`, `enabled` and the per-item states. `import` is
  reserved.
- A dev link is not copied. Every load reads its folder again, so edits show without a
  reinstall. Uninstalling a dev link only forgets it; the folder is never deleted.
- The builtin effects appear in `pack list` as a virtual `builtin` pack that cannot be removed.

Not yet: store search and remote URLs, MCP install and uninstall (the app asks the user to
confirm those), enabling and disabling from the CLI (the library has `set_enabled`), projects
storing refs instead of pack paths (schema v8), and "Collect project".

The starter pack stays a format-1 folder for now: format 2 does not yet load caption styles,
templates, sticker or sfx items, or transition aliases. Its `.rpack` is built from that folder
and installs as `local.starter` 1.0.0.

### Perception

`describe_effects` (MCP) lists the builtins plus the packs installed in the user library, or
the packs you name by folder. `describe_commands` lists the same catalogue. Each line gives the
ref, kind, label, state (with the reason when not `ok`), the usual `slot` and the params: type,
range, default and `initial` (the value written when the effect is added).

For the webview preview (docs/preview-webview.md), `renderly_core::packs::host::host_item`
reads one referenced item's files (a LUT's `.cube` text, or an effect's `effect.json`, WGSL files
and texture files) and the desktop app serves them through `read_preview_pack_items`; the wasm
compositor compiles them with the same code export uses (`WasmCompositor.set_pack_item`).

## Commands

- `LoadAssetPack { path }` — deprecated in v8: installs the pack folder into the user library
  (the project does not record it); `UnloadAssetPack { pack_id }` uninstalls it
- `SetClipEffects` / `SetClipTransition` apply items by ref
- `AddStickerFromPack` / `AddSfxFromPack`
- `ApplyTemplate`

See example: [`examples/packs/starter`](../examples/packs/starter).
Registry seed: [`examples/registry/index.json`](../examples/registry/index.json).
