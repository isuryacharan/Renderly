# WASM plugin API v1 (frame + audio effects)

Sandboxed plugins via [wasmtime](https://wasmtime.dev/) (Apache-2.0 / MIT — AGPL-compatible
when linked). Plugins cannot access the filesystem, network, or host APIs beyond the
exported guest ABI below.

## Layout

```
my-plugin/
  plugin.json
  effect.wasm
```

### `plugin.json`

```json
{
  "id": "invert",
  "name": "Invert Colors",
  "version": "1",
  "wasm": "invert.wasm"
}
```

Effect id when loaded: `wasm:<id>`.

## Guest ABI

Exports (required):

| Export | Signature | Notes |
|--------|-----------|--------|
| `memory` | linear memory | Host writes tightly packed buffers at offset 0 |

Plus at least one of:

| Export | Signature | Notes |
|--------|-----------|--------|
| `process` | `(ptr: i32, len: i32, width: i32, height: i32) -> ()` | Mutate RGBA8 buffer in place |
| `process_audio` | `(ptr, sample_count, sample_rate, channels, intensity_milli) -> ()` | Mutate interleaved f32 PCM; `intensity_milli` is `round(intensity * 1000)` from effect params |

No host imports in v1. The module must not require WASI.

## Host flow

1. `LoadWasmPlugin { path }` validates the manifest, compiles the module, records the folder
   in the project's `legacy_pack_paths` (plugins are not library items yet). Effects name a
   plugin by the ref `wasm:<plugin-id>`; they stay in a clip's `effects[]` list.
2. **Video:** during preview/export, after decode and before GPU upload, enabled `wasm:*`
   frame effects run through `PluginHost::apply_effects`.
3. **Audio:** before FFmpeg amix, clips with audio-capable `wasm:*` effects are decoded to
   f32 PCM, processed via `PluginHost::apply_audio_effects`, then remixed.
4. Builtin GPU effects still run after upload on video.

## Templates

- [`examples/plugins/invert`](../examples/plugins/invert) — frame invert
- [`examples/plugins/gain`](../examples/plugins/gain) — audio gain (`intensity` param)

Regenerate WASM bytes:

```rust
let bytes = renderly_core::compile_invert_wasm()?;
// or compile_gain_wasm()
```

Or: `cargo run -p renderly-core --example write_gain_wasm`.

## Commands

- `LoadWasmPlugin { path }`
- `UnloadWasmPlugin { plugin_id }`
- `SetClipEffects` accepts `wasm:<id>` when the plugin is loaded
