//! Sandboxed WASM frame/audio-effect plugins (wasmtime). See docs/plugin-api.md.

use crate::media::RgbaFrame;
use crate::project::{EffectInstance, Project};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use wasmtime::*;

#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_plugin_version")]
    pub version: String,
    /// Relative path to the `.wasm` module inside the plugin directory.
    pub wasm: String,
}

fn default_plugin_version() -> String {
    "1".into()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PluginCapabilities {
    pub has_frame: bool,
    pub has_audio: bool,
}

pub fn load_plugin_manifest(root: &Path) -> Result<PluginManifest, String> {
    let path = root.join("plugin.json");
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let manifest: PluginManifest =
        serde_json::from_str(&text).map_err(|e| format!("plugin.json: {e}"))?;
    if manifest.id.trim().is_empty() {
        return Err("plugin id must be non-empty".into());
    }
    if !root.join(&manifest.wasm).is_file() {
        return Err(format!("wasm file '{}' not found", manifest.wasm));
    }
    Ok(manifest)
}

pub fn plugin_id_at(root: &Path) -> Option<String> {
    load_plugin_manifest(root).ok().map(|m| m.id)
}

pub fn plugin_ref(plugin_id: &str) -> String {
    format!("wasm:{plugin_id}")
}

/// The project's WASM plugin folders: the entries of `legacy_pack_paths` that hold a
/// `plugin.json` (pack folders are installed into the user library instead).
fn plugin_roots(project: &Project) -> impl Iterator<Item = &Path> {
    project
        .legacy_pack_paths
        .iter()
        .map(|p| p.as_path())
        .filter(|p| p.join("plugin.json").is_file())
}

pub fn known_plugin_refs(project: &Project) -> HashSet<String> {
    let mut ids = HashSet::new();
    for path in plugin_roots(project) {
        if let Ok(m) = load_plugin_manifest(path) {
            ids.insert(plugin_ref(&m.id));
        }
    }
    ids
}

/// Inspect exports without fully constructing a host (for UI catalog).
pub fn plugin_capabilities(root: &Path) -> Result<PluginCapabilities, String> {
    let mut host = PluginHost::empty()?;
    host.add_dir(root)?;
    let plugin = host
        .plugins
        .first()
        .ok_or_else(|| "no plugin".to_string())?;
    Ok(PluginCapabilities {
        has_frame: plugin.has_frame,
        has_audio: plugin.has_audio,
    })
}

/// Host that can run loaded plugins against RGBA frames and/or PCM audio.
pub struct PluginHost {
    engine: Engine,
    plugins: Vec<LoadedPlugin>,
}

struct LoadedPlugin {
    id: String,
    module: Module,
    has_frame: bool,
    has_audio: bool,
    instance: Mutex<Option<(Store<()>, Instance)>>,
}

impl PluginHost {
    pub fn empty() -> Result<Self, String> {
        let mut config = Config::new();
        config.cranelift_opt_level(OptLevel::Speed);
        // No WASI / no host imports in v1 — guest may only touch its linear memory.
        let engine = Engine::new(&config).map_err(|e| e.to_string())?;
        Ok(Self {
            engine,
            plugins: Vec::new(),
        })
    }

    pub fn load_from_dir(root: &Path) -> Result<Self, String> {
        let mut host = Self::empty()?;
        host.add_dir(root)?;
        Ok(host)
    }

    pub fn for_project(project: &Project) -> Result<Self, String> {
        let mut host = Self::empty()?;
        for path in plugin_roots(project) {
            host.add_dir(path)?;
        }
        Ok(host)
    }

    pub fn add_dir(&mut self, root: &Path) -> Result<(), String> {
        let manifest = load_plugin_manifest(root)?;
        let wasm_path = root.join(&manifest.wasm);
        let bytes = std::fs::read(&wasm_path).map_err(|e| e.to_string())?;
        let module = Module::new(&self.engine, &bytes).map_err(|e| format!("wasm: {e}"))?;
        let has_memory = module.exports().any(|e| e.name() == "memory");
        let has_frame = module.exports().any(|e| e.name() == "process");
        let has_audio = module.exports().any(|e| e.name() == "process_audio");
        if !has_memory || (!has_frame && !has_audio) {
            return Err(
                "plugin must export `memory` and at least one of `process` / `process_audio`"
                    .into(),
            );
        }
        self.plugins.retain(|p| p.id != manifest.id);
        self.plugins.push(LoadedPlugin {
            id: manifest.id,
            module,
            has_frame,
            has_audio,
            instance: Mutex::new(None),
        });
        Ok(())
    }

    pub fn plugin_is_audio(&self, plugin_id: &str) -> bool {
        self.plugins
            .iter()
            .find(|p| p.id == plugin_id)
            .map(|p| p.has_audio)
            .unwrap_or(false)
    }

    pub fn plugin_is_frame(&self, plugin_id: &str) -> bool {
        self.plugins
            .iter()
            .find(|p| p.id == plugin_id)
            .map(|p| p.has_frame)
            .unwrap_or(false)
    }

    pub fn apply_effects(
        &self,
        frame: &mut RgbaFrame,
        effects: &[EffectInstance],
    ) -> Result<(), String> {
        for effect in effects.iter().filter(|e| e.enabled) {
            let Some(plugin_id) = effect.item_ref.strip_prefix("wasm:") else {
                continue;
            };
            let Some(plugin) = self.plugins.iter().find(|p| p.id == plugin_id) else {
                continue;
            };
            if !plugin.has_frame {
                continue;
            }
            self.run_process(plugin, frame)?;
        }
        Ok(())
    }

    /// Process interleaved f32 PCM in place for enabled `wasm:*` audio effects.
    pub fn apply_audio_effects(
        &self,
        samples: &mut [f32],
        sample_rate: u32,
        channels: u32,
        effects: &[EffectInstance],
    ) -> Result<(), String> {
        for effect in effects.iter().filter(|e| e.enabled) {
            let Some(plugin_id) = effect.item_ref.strip_prefix("wasm:") else {
                continue;
            };
            let Some(plugin) = self.plugins.iter().find(|p| p.id == plugin_id) else {
                continue;
            };
            if !plugin.has_audio {
                continue;
            }
            let intensity = effect.number("intensity").unwrap_or(1.0) as f32;
            self.run_process_audio(plugin, samples, sample_rate, channels, intensity)?;
        }
        Ok(())
    }

    fn run_process(&self, plugin: &LoadedPlugin, frame: &mut RgbaFrame) -> Result<(), String> {
        let (mut store, instance) = {
            let mut cache = plugin.instance.lock().map_err(|e| e.to_string())?;
            if let Some(pair) = cache.take() {
                pair
            } else {
                let mut store = Store::new(&self.engine, ());
                let instance =
                    Instance::new(&mut store, &plugin.module, &[]).map_err(|e| e.to_string())?;
                (store, instance)
            }
        };

        let result = (|| {
            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| "missing memory export".to_string())?;
            let process = instance
                .get_typed_func::<(i32, i32, i32, i32), ()>(&mut store, "process")
                .map_err(|e| format!("process export: {e}"))?;

            let nbytes = frame.pixels.len();
            let ptr = 0i32;
            let needed = nbytes as u64;
            let pages_needed = needed.div_ceil(65536).max(1);
            let current = memory.size(&store);
            if current < pages_needed {
                memory
                    .grow(&mut store, pages_needed - current)
                    .map_err(|e| format!("memory grow: {e}"))?;
            }
            memory.data_mut(&mut store)[..nbytes].copy_from_slice(&frame.pixels);
            process
                .call(
                    &mut store,
                    (ptr, nbytes as i32, frame.width as i32, frame.height as i32),
                )
                .map_err(|e| format!("process call: {e}"))?;
            frame.pixels.copy_from_slice(&memory.data(&store)[..nbytes]);
            Ok(())
        })();

        // Return to cache if successful
        if result.is_ok() {
            if let Ok(mut cache) = plugin.instance.lock() {
                *cache = Some((store, instance));
            }
        }

        result
    }

    fn run_process_audio(
        &self,
        plugin: &LoadedPlugin,
        samples: &mut [f32],
        sample_rate: u32,
        channels: u32,
        intensity: f32,
    ) -> Result<(), String> {
        let (mut store, instance) = {
            let mut cache = plugin.instance.lock().map_err(|e| e.to_string())?;
            if let Some(pair) = cache.take() {
                pair
            } else {
                let mut store = Store::new(&self.engine, ());
                let instance =
                    Instance::new(&mut store, &plugin.module, &[]).map_err(|e| e.to_string())?;
                (store, instance)
            }
        };

        let result = (|| {
            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| "missing memory export".to_string())?;
            // (ptr, sample_count, sample_rate, channels, intensity_milli)
            let process = instance
                .get_typed_func::<(i32, i32, i32, i32, i32), ()>(&mut store, "process_audio")
                .map_err(|e| format!("process_audio export: {e}"))?;

            let nbytes = std::mem::size_of_val(samples);
            let ptr = 0i32;
            let needed = nbytes as u64;
            let pages_needed = needed.div_ceil(65536).max(1);
            let current = memory.size(&store);
            if current < pages_needed {
                memory
                    .grow(&mut store, pages_needed - current)
                    .map_err(|e| format!("memory grow: {e}"))?;
            }
            {
                let data = memory.data_mut(&mut store);
                let bytes = bytemuck::cast_slice(samples);
                data[..nbytes].copy_from_slice(bytes);
            }
            let intensity_milli = (intensity.clamp(0.0, 8.0) * 1000.0).round() as i32;
            process
                .call(
                    &mut store,
                    (
                        ptr,
                        samples.len() as i32,
                        sample_rate as i32,
                        channels as i32,
                        intensity_milli,
                    ),
                )
                .map_err(|e| format!("process_audio call: {e}"))?;
            {
                let data = memory.data(&store);
                let out: &[f32] = bytemuck::cast_slice(&data[..nbytes]);
                samples.copy_from_slice(out);
            }
            Ok(())
        })();

        // Return to cache if successful
        if result.is_ok() {
            if let Ok(mut cache) = plugin.instance.lock() {
                *cache = Some((store, instance));
            }
        }

        result
    }
}

/// Compile the built-in invert WAT to bytes (also used to seed the example plugin).
pub fn compile_invert_wasm() -> Result<Vec<u8>, String> {
    wat::parse_str(INVERT_WAT).map_err(|e| e.to_string())
}

/// Compile the built-in gain WAT (audio-only) to bytes.
pub fn compile_gain_wasm() -> Result<Vec<u8>, String> {
    wat::parse_str(GAIN_WAT).map_err(|e| e.to_string())
}

const INVERT_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "process") (param $ptr i32) (param $len i32) (param $w i32) (param $h i32)
    (local $i i32)
    (local $v i32)
    (local $mod i32)
    (block $done
      (loop $loop
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $mod (i32.rem_u (local.get $i) (i32.const 4)))
        (if (i32.ne (local.get $mod) (i32.const 3))
          (then
            (local.set $v
              (i32.load8_u (i32.add (local.get $ptr) (local.get $i))))
            (i32.store8
              (i32.add (local.get $ptr) (local.get $i))
              (i32.sub (i32.const 255) (local.get $v)))
          )
        )
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)
      )
    )
  )
)
"#;

/// Multiplies each f32 sample by (intensity_milli / 1000).
const GAIN_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "process_audio")
    (param $ptr i32) (param $count i32) (param $rate i32) (param $ch i32) (param $milli i32)
    (local $i i32)
    (local $off i32)
    (local $gain f32)
    (local $v f32)
    (local.set $gain
      (f32.div (f32.convert_i32_s (local.get $milli)) (f32.const 1000)))
    (block $done
      (loop $loop
        (br_if $done (i32.ge_s (local.get $i) (local.get $count)))
        (local.set $off
          (i32.add (local.get $ptr) (i32.mul (local.get $i) (i32.const 4))))
        (local.set $v (f32.load (local.get $off)))
        (f32.store (local.get $off) (f32.mul (local.get $v) (local.get $gain)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)
      )
    )
  )
)
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::RgbaFrame;

    #[test]
    fn invert_wasm_flips_rgb() {
        let wasm = compile_invert_wasm().unwrap();
        let dir = std::env::temp_dir().join(format!("renderly-wasm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("invert.wasm"), &wasm).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"id":"invert","name":"Invert","wasm":"invert.wasm"}"#,
        )
        .unwrap();

        let host = PluginHost::load_from_dir(&dir).unwrap();
        let mut frame = RgbaFrame {
            width: 2,
            height: 1,
            pixels: vec![10, 20, 30, 255, 40, 50, 60, 128],
        };
        let effect = EffectInstance {
            id: uuid::Uuid::new_v4(),
            item_ref: "wasm:invert".into(),
            hash: None,
            enabled: true,
            params: Default::default(),
        };
        host.apply_effects(&mut frame, &[effect]).unwrap();
        assert_eq!(frame.pixels, vec![245, 235, 225, 255, 215, 205, 195, 128]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gain_wasm_scales_samples() {
        let wasm = compile_gain_wasm().unwrap();
        let dir = std::env::temp_dir().join(format!("renderly-gain-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gain.wasm"), &wasm).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"id":"gain","name":"Gain","wasm":"gain.wasm"}"#,
        )
        .unwrap();

        let host = PluginHost::load_from_dir(&dir).unwrap();
        assert!(host.plugin_is_audio("gain"));
        assert!(!host.plugin_is_frame("gain"));
        let mut samples = vec![0.5f32, -0.25, 1.0, 0.0];
        let effect = EffectInstance {
            id: uuid::Uuid::new_v4(),
            item_ref: "wasm:gain".into(),
            hash: None,
            enabled: true,
            params: [(
                "intensity".to_string(),
                crate::project::EffectParam::Number(0.5),
            )]
            .into_iter()
            .collect(),
        };
        host.apply_audio_effects(&mut samples, 48000, 2, &[effect])
            .unwrap();
        assert!((samples[0] - 0.25).abs() < 1e-5);
        assert!((samples[1] - -0.125).abs() < 1e-5);
        assert!((samples[2] - 0.5).abs() < 1e-5);
        assert!((samples[3] - 0.0).abs() < 1e-5);
        std::fs::remove_dir_all(&dir).ok();
    }
}
