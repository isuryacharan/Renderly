//! wasm-bindgen bindings exposing `renderly-core`'s GPU compositor to the webview preview.
//!
//! This crate exists so wasm-bindgen stays out of `renderly-core`'s default build (see
//! docs/preview-webview.md "Bindings"). It compiles to something useful only on
//! `wasm32-unknown-unknown` (built with `wasm-pack build --target web`, see the npm
//! `build:wasm` script in renderly-app); on native targets it is an intentionally empty
//! lib so plain `cargo build --workspace` stays green.
//!
//! The exported surface is [`WasmCompositor`]: `create(canvas)` (async — requests a WebGPU
//! adapter/device and configures a surface on the canvas), `set_project(json)`, and
//! `render(time_us, sources)` where `sources` maps media-id strings to the browser
//! `<video>`/`<img>` elements the P1 preview engine already manages. Frames travel
//! element → GPU texture via `Queue::copy_external_image_to_texture` (no CPU readback),
//! then through the exact same `compose::eval` + `Compositor` code the native export uses.

#[cfg(target_arch = "wasm32")]
mod compositor;

#[cfg(target_arch = "wasm32")]
pub use compositor::WasmCompositor;
