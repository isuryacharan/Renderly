//! Pack format 2 loading (docs/effect-pack-format.md, implementation step 3).
//!
//! - Item states: ok, skipped (unknown kind), disabled (needs a feature this build lacks),
//!   error (broken WGSL). A failing item never stops the rest of the pack.
//! - Hashes: stable across loads, changed by an edit to a referenced file, and reused from the
//!   `(path, mtime, size)` cache when the file looks unchanged.
//! - Refs resolve to ok, changed, missing or disabled; format-1 ids answer to `local.<id>`.
//! - Perception lists pack items beside the builtins.
//! - A loaded pack effect renders offscreen through the runtime executor.
//!
//! GPU tests skip (eprintln + return) when no wgpu adapter is available, like the others.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::{Rgba, RgbaImage};
use renderly_core::compose::runtime::{
    CompiledEffect, EffectExecutor, EffectInputs, HostValues, ParamValue,
};
use renderly_core::packs::describe::{catalogue, catalogue_text, pack_report};
use renderly_core::packs::refs::{resolve, ItemRef, Resolution};
use renderly_core::packs::{find_cube_lut, load_pack, ItemState, LoadedItem, LoadedPack, FEATURES};
use serde_json::json;

const W: u32 = 64;
const H: u32 = 32;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/packs")
        .join(name)
}

/// A scratch copy of a fixture, so a test can edit files without touching the repo.
fn scratch_copy(name: &str) -> PathBuf {
    let dst = std::env::temp_dir().join(format!("renderly-pack-test-{}", uuid::Uuid::new_v4()));
    copy_dir(&fixture(name), &dst);
    dst
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn item<'a>(pack: &'a LoadedPack, id: &str, version: u32) -> &'a LoadedItem {
    pack.items
        .iter()
        .find(|i| i.id == id && i.version == version)
        .unwrap_or_else(|| panic!("no item {id}@{version} in {}", pack.canonical_id))
}

fn hash_of(pack: &LoadedPack, id: &str, version: u32) -> String {
    item(pack, id, version)
        .hash
        .clone()
        .unwrap_or_else(|| panic!("{id}@{version} has no hash"))
}

/// A scratch copy of the looks fixture whose `pack.json` is replaced by `body`.
fn pack_dir(body: &serde_json::Value) -> PathBuf {
    let dir = scratch_copy("looks-v2");
    std::fs::write(dir.join("pack.json"), body.to_string()).unwrap();
    dir
}

fn looks_pack_json() -> serde_json::Value {
    let text = std::fs::read_to_string(fixture("looks-v2").join("pack.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn format_2_pack_loads_each_item_with_its_state() {
    let pack = load_pack(&fixture("looks-v2")).unwrap();
    assert_eq!(pack.format, 2);
    assert_eq!(pack.canonical_id, "fixture.looks");
    assert_eq!(pack.meta.license.as_deref(), Some("CC0-1.0"));
    assert_eq!(pack.items.len(), 7, "every declared item is listed");

    for (id, version) in [("tint", 1), ("tint", 2), ("wash", 1), ("cool", 1)] {
        let i = item(&pack, id, version);
        assert_eq!(i.state, ItemState::Ok, "{id}@{version}");
        assert!(i.hash.is_some(), "{id}@{version} has a hash");
    }
    assert!(item(&pack, "tint", 1).compiled.is_some());
    assert!(
        item(&pack, "cool", 1).compiled.is_none(),
        "LUTs are not compiled effects"
    );
    assert_eq!(item(&pack, "wash", 1).kind, "transition");

    // The inline LUT is reachable by the legacy id too.
    let packs = std::slice::from_ref(&pack);
    assert!(find_cube_lut(packs, "fixture.looks", "cool").is_some());

    let glitter = pack.items.iter().find(|i| i.id == "glitter").unwrap();
    assert_eq!(
        glitter.state,
        ItemState::Skipped {
            reason: "needs a newer Renderly (item kind `sparkle`)".into()
        }
    );

    let shimmer = item(&pack, "shimmer", 1);
    assert!(
        matches!(&shimmer.state, ItemState::Disabled { reason } if reason == "needs a newer Renderly (feature `gpu.compute`)"),
        "{:?}",
        shimmer.state
    );
    assert!(shimmer.hash.is_none() && shimmer.compiled.is_none());

    let broken = item(&pack, "broken", 1);
    assert!(
        matches!(&broken.state, ItemState::Error { message } if !message.is_empty()),
        "{:?}",
        broken.state
    );
    assert!(broken.hash.is_none());
}

#[test]
fn hashes_are_stable_and_change_only_for_the_edited_item() {
    let dir = scratch_copy("looks-v2");
    let first = load_pack(&dir).unwrap();
    let again = load_pack(&dir).unwrap();
    assert_eq!(hash_of(&first, "tint", 1), hash_of(&again, "tint", 1));
    assert_eq!(hash_of(&first, "tint", 1).len(), 64);
    assert_ne!(hash_of(&first, "tint", 1), hash_of(&first, "tint", 2));

    let shader = dir.join("effects/tint/1/tint.wgsl");
    let edited = format!("{}\n// edited\n", std::fs::read_to_string(&shader).unwrap());
    std::fs::write(&shader, edited).unwrap();
    let after = load_pack(&dir).unwrap();
    assert_ne!(hash_of(&first, "tint", 1), hash_of(&after, "tint", 1));
    assert_eq!(hash_of(&first, "tint", 2), hash_of(&after, "tint", 2));
    assert_eq!(hash_of(&first, "wash", 1), hash_of(&after, "wash", 1));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_unchanged_mtime_and_size_reuses_the_cached_digest() {
    let dir = scratch_copy("looks-v2");
    let shader = dir.join("effects/tint/1/tint.wgsl");
    let original = std::fs::read_to_string(&shader).unwrap();
    let mtime = std::fs::metadata(&shader).unwrap().modified().unwrap();
    let first = hash_of(&load_pack(&dir).unwrap(), "tint", 1);

    // Same length, different bytes, original mtime restored. The cache is keyed on (path,
    // mtime, size), so it still answers with the digest it computed: that is the trade the
    // design accepts for not rehashing unchanged packs.
    let same_size = original.replacen("0.6", "0.7", 1);
    assert_eq!(same_size.len(), original.len());
    std::fs::write(&shader, &same_size).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&shader)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(first, hash_of(&load_pack(&dir).unwrap(), "tint", 1));

    // A longer file is a cache miss, so the new content is hashed.
    std::fs::write(&shader, format!("{same_size}\n// longer\n")).unwrap();
    assert_ne!(first, hash_of(&load_pack(&dir).unwrap(), "tint", 1));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn refs_resolve_to_ok_changed_missing_or_disabled() {
    let packs = vec![
        load_pack(&fixture("looks-v2")).unwrap(),
        load_pack(&fixture("starter-v1")).unwrap(),
    ];
    let tint_hash = hash_of(&packs[0], "tint", 1);
    let r = |s: &str| ItemRef::parse(s).unwrap();

    assert!(matches!(
        resolve(&packs, &r("pack:fixture.looks/tint@1"), Some(&tint_hash)),
        Resolution::Ok(_)
    ));
    assert!(matches!(
        resolve(&packs, &r("pack:fixture.looks/tint@1"), None),
        Resolution::Ok(_)
    ));
    match resolve(
        &packs,
        &r("pack:fixture.looks/tint@1"),
        Some(&"0".repeat(64)),
    ) {
        Resolution::Changed {
            installed_hash,
            item,
        } => {
            assert_eq!(installed_hash, tint_hash);
            assert_eq!(item.version, 1);
        }
        other => panic!("expected Changed, got {other:?}"),
    }
    assert!(matches!(
        resolve(&packs, &r("pack:fixture.looks/tint@3"), None),
        Resolution::Missing
    ));
    assert!(matches!(
        resolve(&packs, &r("pack:someone.else/tint@1"), None),
        Resolution::Missing
    ));
    match resolve(&packs, &r("pack:fixture.looks/shimmer@1"), None) {
        Resolution::Disabled { reason } => assert!(reason.contains("gpu.compute"), "{reason}"),
        other => panic!("expected Disabled, got {other:?}"),
    }
    match resolve(&packs, &r("pack:fixture.looks/broken@1"), None) {
        Resolution::Disabled { reason } => assert!(reason.contains("failed to load"), "{reason}"),
        other => panic!("expected Disabled, got {other:?}"),
    }
    // Two versions side by side: each ref finds its own item.
    assert_ne!(
        hash_of(&packs[0], "tint", 2),
        tint_hash,
        "v2 is a different picture, so a different hash"
    );
    // Format-1 ids answer to both spellings; builtins resolve too.
    assert!(matches!(
        resolve(&packs, &r("pack:starter/punch@1"), None),
        Resolution::Ok(_)
    ));
    assert!(matches!(
        resolve(&packs, &r("pack:local.starter/punch@1"), None),
        Resolution::Ok(_)
    ));
    assert!(matches!(
        resolve(&packs, &r("builtin:blur@1"), None),
        Resolution::Ok(_)
    ));
}

#[test]
fn format_1_packs_keep_loading_and_show_up_as_items() {
    let pack = load_pack(&fixture("starter-v1")).unwrap();
    assert_eq!(pack.format, 1);
    assert_eq!(pack.manifest.id, "starter", "format-1 ids are unchanged");
    assert_eq!(pack.canonical_id, "local.starter");
    assert!(find_cube_lut(std::slice::from_ref(&pack), "starter", "punch").is_some());

    let punch = item(&pack, "punch", 1);
    assert_eq!(punch.kind, "lut");
    assert_eq!(punch.state, ItemState::Ok);
    let quick = item(&pack, "quick-wipe", 1);
    assert_eq!(quick.kind, "transition");
    assert_eq!(quick.alias_of.as_deref(), Some("wipe_left"));
}

#[test]
fn an_unknown_pack_format_refuses_the_whole_pack() {
    let dir = pack_dir(&json!({
        "format": 3, "id": "x.y", "name": "X", "release": "1.0.0", "license": "CC0-1.0", "items": []
    }));
    let err = load_pack(&dir).unwrap_err();
    assert!(err.contains("unsupported pack format 3"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn pack_level_problems_refuse_the_pack_and_item_problems_do_not() {
    let mut no_license = looks_pack_json();
    no_license.as_object_mut().unwrap().remove("license");
    let err = load_pack(&pack_dir(&no_license)).unwrap_err();
    assert!(err.contains("license"), "{err}");

    let mut bad_release = looks_pack_json();
    bad_release["release"] = json!("1.0");
    assert!(load_pack(&pack_dir(&bad_release)).is_err());
}

#[test]
fn a_pack_that_needs_a_newer_renderly_is_installed_with_its_items_disabled() {
    let mut body = looks_pack_json();
    body["requires"] = json!({ "renderly": ">=99.0" });
    let pack = load_pack(&pack_dir(&body)).unwrap();
    assert_eq!(
        item(&pack, "tint", 1).state,
        ItemState::Disabled {
            reason: "needs a newer Renderly (>=99.0)".into()
        }
    );
    assert!(
        pack.items.iter().all(|i| i.state != ItemState::Ok),
        "no item of an incompatible pack is usable"
    );
    assert!(
        pack.items
            .iter()
            .any(|i| matches!(i.state, ItemState::Skipped { .. })),
        "skipped kinds stay skipped"
    );
}

#[test]
fn duplicate_versions_and_unsupported_lut_layouts_are_item_errors() {
    let mut body = looks_pack_json();
    body["items"] = json!([
        { "kind": "effect", "path": "effects/tint/1" },
        { "kind": "effect", "path": "effects/tint/1" },
        { "kind": "lut", "id": "print", "path": "luts/print.png", "layout": "grid" },
        { "kind": "lut", "id": "cool", "path": "luts/cool.cube", "layout": "strip" }
    ]);
    let dir = pack_dir(&body);
    let pack = load_pack(&dir).unwrap();
    assert_eq!(pack.items[0].state, ItemState::Ok);
    match &pack.items[1].state {
        ItemState::Error { message } => {
            assert!(message.contains("duplicate item tint@1"), "{message}")
        }
        other => panic!("expected duplicate error, got {other:?}"),
    }
    match &pack.items[2].state {
        ItemState::Error { message } => assert!(message.contains("not supported yet"), "{message}"),
        other => panic!("expected a PNG layout error, got {other:?}"),
    }
    match &pack.items[3].state {
        ItemState::Error { message } => assert!(message.contains("not supported yet"), "{message}"),
        other => panic!("expected a strip layout error, got {other:?}"),
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn features_are_the_v1_set() {
    assert_eq!(FEATURES, &["gpu.fragment", "expr.v1"]);
}

#[test]
fn perception_lists_pack_items_beside_the_builtins() {
    let packs = [load_pack(&fixture("looks-v2")).unwrap()];
    let text = catalogue_text(&packs);
    assert!(
        text.contains(
            "pack:fixture.looks/tint@1 effect \"Tint\" ok slot=effects | amount:float 0..1 =0.0 init=0.5"
        ),
        "{text}"
    );
    assert!(text.contains("pack:fixture.looks/tint@2 effect"), "{text}");
    assert!(
        text.contains("pack:fixture.looks/wash@1 transition \"Wash\" ok"),
        "{text}"
    );
    assert!(
        text.contains("pack:fixture.looks/cool@1 lut \"Cool\" ok slot=filter"),
        "{text}"
    );
    assert!(
        text.contains("pack:fixture.looks/shimmer@1 effect \"Shimmer (needs compute)\" disabled: needs a newer Renderly (feature `gpu.compute`)"),
        "{text}"
    );
    assert!(text.contains("builtin:blur@1 effect"), "{text}");

    let entries = catalogue(&packs);
    let tint = entries
        .iter()
        .find(|e| e.reference == "pack:fixture.looks/tint@1")
        .unwrap();
    assert_eq!(tint.source, "fixture.looks");
    assert_eq!(tint.params[0].initial, json!(0.5));

    let report = pack_report(&packs[0]);
    let glitter = report.items.iter().find(|i| i.id == "glitter").unwrap();
    assert_eq!(glitter.state, "skipped");
    let broken = report.items.iter().find(|i| i.id == "broken").unwrap();
    assert_eq!(broken.state, "error");
    assert!(broken.hash.is_none());
    assert!(report
        .items
        .iter()
        .find(|i| i.id == "tint")
        .unwrap()
        .hash
        .is_some());
}

#[test]
fn a_loaded_pack_effect_renders_offscreen() {
    let Some(gpu) = gpu() else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let pack = load_pack(&fixture("looks-v2")).unwrap();
    let effect = item(&pack, "tint", 1)
        .compiled
        .clone()
        .expect("tint@1 compiled");
    let (device, queue) = gpu;
    let texture = upload(device, queue, &solid(200, 200, 200));
    let view = texture.create_view(&Default::default());
    let mut exec = EffectExecutor::new(device);

    let host = HostValues {
        width: W,
        height: H,
        time: 0.0,
        duration: 1.0,
        progress: 0.0,
        frame: 0,
        seed: 1,
        speed: 1.0,
    };
    let amount = |v: f64| BTreeMap::from([("amount".to_string(), ParamValue::Number(v))]);

    // amount 1: green and blue scaled by 0.6, red untouched.
    let tinted = render(gpu, &mut exec, &effect, &view, &amount(1.0), &host);
    assert_close(&tinted[..3], [200, 120, 120]);
    // amount 0: the identity.
    let untouched = render(gpu, &mut exec, &effect, &view, &amount(0.0), &host);
    assert_close(&untouched[..3], [200, 200, 200]);
}

fn assert_close(px: &[u8], want: [u8; 3]) {
    for c in 0..3 {
        assert!(
            (i32::from(px[c]) - i32::from(want[c])).abs() <= 2,
            "channel {c}: got {px:?}, want {want:?}"
        );
    }
}

fn solid(r: u8, g: u8, b: u8) -> RgbaImage {
    RgbaImage::from_pixel(W, H, Rgba([r, g, b, 255]))
}

type Gpu = (wgpu::Device, wgpu::Queue);

fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("effect-packs-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()
    })
    .as_ref()
}

fn upload(device: &wgpu::Device, queue: &wgpu::Queue, img: &RgbaImage) -> wgpu::Texture {
    let (w, h) = img.dimensions();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pack-test-source"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let row = w as usize * 4;
    let pitch = row.div_ceil(256) * 256;
    let mut bytes = vec![0u8; pitch * h as usize];
    for (y, line) in img.as_raw().chunks_exact(row).enumerate() {
        bytes[y * pitch..y * pitch + row].copy_from_slice(line);
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(pitch as u32),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Runs `effect` once on `src`, reads the output back as packed RGBA8 and recycles it.
fn render(
    gpu: &Gpu,
    exec: &mut EffectExecutor,
    effect: &CompiledEffect,
    src: &wgpu::TextureView,
    params: &BTreeMap<String, ParamValue>,
    host: &HostValues,
) -> Vec<u8> {
    let (device, queue) = gpu;
    let mut bound = EffectInputs::new();
    bound.bind("src", src);
    let mut encoder = device.create_command_encoder(&Default::default());
    let out = exec
        .run(
            device,
            queue,
            &mut encoder,
            effect,
            &bound,
            &|k| params.get(k).cloned(),
            host,
        )
        .unwrap_or_else(|e| panic!("run failed: {e}"));
    let (w, h) = (out.width(), out.height());
    let row = w as usize * 4;
    let pitch = row.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pack-test-readback"),
        size: (pitch * h as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: out.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch as u32),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap_or_else(|e| panic!("poll: {e:?}"));
    rx.recv()
        .unwrap_or_else(|_| panic!("readback channel closed"))
        .unwrap_or_else(|e| panic!("map: {e:?}"));
    let mapped = slice
        .get_mapped_range()
        .unwrap_or_else(|e| panic!("range: {e}"));
    let mut out_px = vec![0u8; row * h as usize];
    for y in 0..h as usize {
        out_px[y * row..(y + 1) * row].copy_from_slice(&mapped[y * pitch..y * pitch + row]);
    }
    drop(mapped);
    buffer.unmap();
    exec.recycle(out);
    out_px
}
