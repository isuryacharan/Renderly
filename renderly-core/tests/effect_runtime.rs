//! Effect runtime proof (docs/effect-pack-format.md, implementation step 1).
//!
//! - The builtin `blur` (multi-pass) and `crossfade` (transition), written as pack format 2
//!   items, run through `compose::runtime` and are compared with the committed golden frames
//!   that the hard-coded builtins produce (`fx_blur_t0500ms`, `transition_crossfade_t{1250,
//!   1500,1750}ms`). The source pixels are the golden suite's own generators (copied here so
//!   the test needs no ffmpeg), so the only thing under test is the runtime.
//! - Seek purity: frame N rendered on a fresh executor equals frame N rendered after frames
//!   0..N-1 on the same executor (pooled targets and cached pipelines included).
//! - LUT upload and sampling, and a two-pass item with a half-scale `rgba16f` target read
//!   through `prev`.
//!
//! Skips (eprintln + return) when no wgpu adapter is available, like the other GPU tests.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use image::{Rgba, RgbaImage};
use renderly_core::compose::builtins::{builtin_sources, load_builtin};
use renderly_core::compose::runtime::lut::upload_cube;
use renderly_core::compose::runtime::prelude::UniformLayout;
use renderly_core::compose::runtime::{
    CompiledEffect, EffectExecutor, EffectInputs, EffectManifest, HostValues, ParamValue,
};
use renderly_core::packs::CubeLut;

const W: u32 = 320;
const H: u32 = 180;

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
            label: Some("effect-runtime-test"),
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

macro_rules! gpu_or_skip {
    () => {
        match gpu() {
            Some(g) => g,
            None => {
                eprintln!("skipped: no wgpu adapter");
                return;
            }
        }
    };
}

// The golden suite's source generators (tests/golden_frames.rs), copied verbatim.
fn gen_bars() -> RgbaImage {
    const BARS: [[u8; 3]; 8] = [
        [235, 235, 235],
        [235, 235, 16],
        [16, 235, 235],
        [16, 235, 16],
        [235, 16, 235],
        [235, 16, 16],
        [16, 16, 235],
        [30, 30, 30],
    ];
    RgbaImage::from_fn(320, 180, |x, y| {
        if x + y < 28 {
            return Rgba([0, 0, 0, 255]);
        }
        if (296..312).contains(&x) && (150..166).contains(&y) {
            return Rgba([255, 255, 255, 255]);
        }
        if y < 120 {
            let [r, g, b] = BARS[(x / 40) as usize];
            Rgba([r, g, b, 255])
        } else if y < 150 {
            let v = (x * 255 / 319) as u8;
            Rgba([v, v, v, 255])
        } else {
            let r = (x * 255 / 319) as u8;
            let g = ((y - 150) * 255 / 29) as u8;
            Rgba([r, g, 255 - r, 255])
        }
    })
}

fn gen_stripes() -> RgbaImage {
    RgbaImage::from_fn(320, 180, |x, y| {
        if x % 80 < 2 || y % 60 < 2 {
            Rgba([255, 255, 255, 255])
        } else if ((x + y) / 16) % 2 == 0 {
            Rgba([250, 120, 20, 255])
        } else {
            Rgba([60, 20, 140, 255])
        }
    })
}

fn golden(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/golden/{name}.png", env!("CARGO_MANIFEST_DIR"));
    let img = image::open(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let rgba = img.to_rgba8();
    assert_eq!(rgba.dimensions(), (W, H), "{name} has the wrong size");
    rgba.into_raw()
}

/// The golden suite's default tolerance (tests/golden_frames.rs `GOLDEN_TOL`). The goldens
/// were rendered on one GPU; other backends (Vulkan/lavapipe/Metal on CI) differ by a few
/// LSB in sampling and float rounding. On the machine that made the goldens the runtime
/// renders them exactly (max diff 0), which the eprintln below reports.
const GOLDEN_CHANNEL_TOL: u8 = 3;
const GOLDEN_MAX_FRAC: f64 = 0.001;

fn assert_within_golden_tol(name: &str, actual: &[u8], expected: &[u8]) {
    let (max, count) = diff(actual, expected);
    let over = actual
        .chunks(4)
        .zip(expected.chunks(4))
        .filter(|(a, e)| {
            a.iter()
                .zip(e.iter())
                .any(|(x, y)| x.abs_diff(*y) > GOLDEN_CHANNEL_TOL)
        })
        .count();
    let frac = over as f64 / (actual.len() / 4) as f64;
    eprintln!(
        "{name}: max abs diff {max}, differing channel values {count}, pixels over tol {over}"
    );
    assert!(
        frac <= GOLDEN_MAX_FRAC,
        "{name}: {over} pixels differ by more than {GOLDEN_CHANNEL_TOL} (budget {GOLDEN_MAX_FRAC})"
    );
}

/// Max per-channel abs difference and the number of differing channel values.
fn diff(actual: &[u8], expected: &[u8]) -> (u8, usize) {
    assert_eq!(actual.len(), expected.len());
    let mut max = 0u8;
    let mut count = 0usize;
    for (a, e) in actual.iter().zip(expected) {
        let d = a.abs_diff(*e);
        if d > 0 {
            count += 1;
        }
        max = max.max(d);
    }
    (max, count)
}

fn upload(device: &wgpu::Device, queue: &wgpu::Queue, img: &RgbaImage) -> wgpu::Texture {
    let (w, h) = img.dimensions();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-source"),
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

fn host(time: f64, progress: f64, frame: u32) -> HostValues {
    HostValues {
        width: W,
        height: H,
        time,
        duration: 3.0,
        progress,
        frame,
        seed: 7,
        speed: 1.0,
    }
}

/// Runs `effect` once, reads its output back as tightly packed RGBA8 and recycles it.
fn render(
    gpu: &Gpu,
    exec: &mut EffectExecutor,
    effect: &CompiledEffect,
    inputs: &[(&str, &wgpu::TextureView)],
    params: &BTreeMap<String, ParamValue>,
    host: &HostValues,
) -> Vec<u8> {
    let (device, queue) = gpu;
    let mut bound = EffectInputs::new();
    for (name, view) in inputs {
        bound.bind(name, view);
    }
    let mut encoder = device.create_command_encoder(&Default::default());
    let out = exec
        .run(device, queue, &mut encoder, effect, &bound, params, host)
        .unwrap_or_else(|e| panic!("run failed: {e}"));
    let (w, h) = (out.width(), out.height());
    let row = w as usize * 4;
    let pitch = row.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-readback"),
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

fn blur_params(radius: f64) -> BTreeMap<String, ParamValue> {
    BTreeMap::from([("radius".to_string(), ParamValue::Number(radius))])
}

// ---- format and layout (no GPU) ----------------------------------------------------------

#[test]
fn builtin_items_load_and_their_uniform_layout_matches_naga() {
    let blur = load_builtin("blur").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(blur.manifest().id, "blur");
    let crossfade = load_builtin("crossfade").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(crossfade.manifest().passes.len(), 1);

    // The generated Params struct must agree with the byte layout the executor writes.
    let (json, _) = builtin_sources("blur").unwrap_or_else(|| panic!("blur sources"));
    let manifest = EffectManifest::parse(json).unwrap_or_else(|e| panic!("{e}"));
    let layout = UniformLayout::new(&manifest.uniform_fields());
    let wgsl = blur.pass_wgsl(0).unwrap_or_else(|| panic!("pass 0"));
    let module = wgpu::naga::front::wgsl::parse_str(wgsl).unwrap_or_else(|e| panic!("{e}"));
    let (_, params_ty) = module
        .types
        .iter()
        .find(|(_, t)| t.name.as_deref() == Some("Params"))
        .unwrap_or_else(|| panic!("Params struct"));
    let wgpu::naga::TypeInner::Struct { members, span } = &params_ty.inner else {
        panic!("Params is not a struct");
    };
    assert_eq!(*span as usize, layout.size);
    let named: Vec<(String, u32)> = members
        .iter()
        .map(|m| (m.name.clone().unwrap_or_default(), m.offset))
        .collect();
    let expected: Vec<(String, u32)> = layout
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.offset as u32))
        .collect();
    assert_eq!(named, expected);
}

#[test]
fn host_block_layout_matches_naga() {
    let blur = load_builtin("blur").unwrap_or_else(|e| panic!("{e}"));
    let wgsl = blur.pass_wgsl(0).unwrap_or_else(|| panic!("pass 0"));
    let module = wgpu::naga::front::wgsl::parse_str(wgsl).unwrap_or_else(|e| panic!("{e}"));
    let (_, host_ty) = module
        .types
        .iter()
        .find(|(_, t)| t.name.as_deref() == Some("Host"))
        .unwrap_or_else(|| panic!("Host struct"));
    let wgpu::naga::TypeInner::Struct { members, span } = &host_ty.inner else {
        panic!("Host is not a struct");
    };
    assert_eq!(
        *span as usize,
        renderly_core::compose::runtime::prelude::HOST_SIZE
    );
    let offsets: Vec<u32> = members.iter().map(|m| m.offset).collect();
    assert_eq!(offsets, vec![0, 8, 16, 20, 24, 28, 32, 36]);
}

// ---- golden parity -----------------------------------------------------------------------

#[test]
fn blur_matches_the_committed_golden_frame() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    let blur = load_builtin("blur").unwrap_or_else(|e| panic!("{e}"));
    let src = upload(device, queue, &gen_bars());
    let view = src.create_view(&Default::default());
    let mut exec = EffectExecutor::new(device);
    // fx_blur_t0500ms: a single full-frame bars clip at 0.5 s with blur radius 6.
    let actual = render(
        gpu,
        &mut exec,
        &blur,
        &[("src", &view)],
        &blur_params(6.0),
        &host(0.5, 0.5 / 3.0, 15),
    );
    let expected = golden("fx_blur_t0500ms");
    // The check has teeth: the blurred golden is not the unblurred source.
    assert_ne!(diff(&expected, &gen_bars().into_raw()).1, 0);
    assert_within_golden_tol("fx_blur_t0500ms", &actual, &expected);
}

#[test]
fn crossfade_matches_the_committed_golden_frames() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    let crossfade = load_builtin("crossfade").unwrap_or_else(|e| panic!("{e}"));
    let from = upload(device, queue, &gen_bars());
    let to = upload(device, queue, &gen_stripes());
    let from_view = from.create_view(&Default::default());
    let to_view = to.create_view(&Default::default());
    let mut exec = EffectExecutor::new(device);
    // Crossfade over [1.0 s, 2.0 s): progress = (t - 1.0) / 1.0 at the golden sample times.
    for (t_ms, progress) in [(1250u32, 0.25), (1500, 0.5), (1750, 0.75)] {
        let actual = render(
            gpu,
            &mut exec,
            &crossfade,
            &[("from", &from_view), ("to", &to_view)],
            &BTreeMap::new(),
            &host(f64::from(t_ms) / 1000.0, progress, 0),
        );
        let name = format!("transition_crossfade_t{t_ms:04}ms");
        assert_within_golden_tol(&name, &actual, &golden(&name));
    }
}

// ---- seek purity -------------------------------------------------------------------------

#[test]
fn blur_frame_n_is_the_same_cold_or_after_playing_up_to_it() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    let blur = load_builtin("blur").unwrap_or_else(|e| panic!("{e}"));
    let src = upload(device, queue, &gen_bars());
    let view = src.create_view(&Default::default());
    let radius = |n: u32| 1.0 + f64::from(n) * 2.0;
    let n = 4u32;

    let mut cold = EffectExecutor::new(device);
    let cold_frame = render(
        gpu,
        &mut cold,
        &blur,
        &[("src", &view)],
        &blur_params(radius(n)),
        &host(f64::from(n) / 30.0, 0.0, n),
    );

    let mut warm = EffectExecutor::new(device);
    for k in 0..n {
        let _ = render(
            gpu,
            &mut warm,
            &blur,
            &[("src", &view)],
            &blur_params(radius(k)),
            &host(f64::from(k) / 30.0, 0.0, k),
        );
    }
    let warm_frame = render(
        gpu,
        &mut warm,
        &blur,
        &[("src", &view)],
        &blur_params(radius(n)),
        &host(f64::from(n) / 30.0, 0.0, n),
    );
    assert_eq!(diff(&cold_frame, &warm_frame), (0, 0));
}

#[test]
fn crossfade_frame_n_is_the_same_cold_or_after_playing_up_to_it() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    let crossfade = load_builtin("crossfade").unwrap_or_else(|e| panic!("{e}"));
    let from = upload(device, queue, &gen_bars());
    let to = upload(device, queue, &gen_stripes());
    let from_view = from.create_view(&Default::default());
    let to_view = to.create_view(&Default::default());
    let progress = |k: u32| f64::from(k) / 8.0;
    let n = 5u32;
    let inputs = [("from", &from_view), ("to", &to_view)];

    let mut cold = EffectExecutor::new(device);
    let cold_frame = render(
        gpu,
        &mut cold,
        &crossfade,
        &inputs,
        &BTreeMap::new(),
        &host(0.0, progress(n), n),
    );

    let mut warm = EffectExecutor::new(device);
    for k in 0..n {
        let _ = render(
            gpu,
            &mut warm,
            &crossfade,
            &inputs,
            &BTreeMap::new(),
            &host(0.0, progress(k), k),
        );
    }
    let warm_frame = render(
        gpu,
        &mut warm,
        &crossfade,
        &inputs,
        &BTreeMap::new(),
        &host(0.0, progress(n), n),
    );
    assert_eq!(diff(&cold_frame, &warm_frame), (0, 0));
}

// ---- LUTs and multi-pass -----------------------------------------------------------------

const LUT_JSON: &str = r#"{
  "id": "lut-test", "version": 1, "kind": "filter", "label": "LUT test",
  "params": [
    { "key": "lut", "type": "lut", "label": "LUT" },
    { "key": "intensity", "type": "float", "min": 0, "max": 1, "default": 1, "label": "Intensity" }
  ],
  "passes": [ { "shader": "lut.wgsl", "inputs": ["src", "lut"], "output": "out" } ]
}"#;

const LUT_WGSL: &str = r#"
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let c = textureSample(src, samp_linear, in.uv);
    let n = f32(textureDimensions(lut).x);
    let uvw = clamp(c.rgb, vec3f(0.0), vec3f(1.0)) * ((n - 1.0) / n) + vec3f(0.5 / n);
    let graded = textureSample(lut, samp_linear, uvw).rgb;
    return vec4f(mix(c.rgb, graded, params.intensity), c.a);
}
"#;

fn cube(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> CubeLut {
    let mut data = Vec::with_capacity(size * size * size);
    let d = (size - 1) as f32;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                data.push(f([r as f32 / d, g as f32 / d, b as f32 / d]));
            }
        }
    }
    CubeLut { size, data }
}

#[test]
fn lut_identity_and_inversion_sample_correctly() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    let shaders = BTreeMap::from([("lut.wgsl".to_string(), LUT_WGSL.to_string())]);
    let effect = CompiledEffect::load(LUT_JSON, &shaders, &BTreeMap::new())
        .unwrap_or_else(|e| panic!("{e}"));
    let src = upload(device, queue, &gen_bars());
    let view = src.create_view(&Default::default());
    let source = gen_bars().into_raw();
    let mut exec = EffectExecutor::new(device);

    let identity = upload_cube(device, queue, &cube(17, |c| c)).unwrap_or_else(|e| panic!("{e}"));
    let params = BTreeMap::from([("lut".to_string(), ParamValue::Lut("identity".into()))]);
    let out = render_with_lut(gpu, &mut exec, &effect, &view, identity.view(), &params);
    let worst = out
        .iter()
        .zip(&source)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(worst <= 2, "identity LUT moved a value by {worst}");

    let inverted = upload_cube(
        device,
        queue,
        &cube(17, |c| [1.0 - c[0], 1.0 - c[1], 1.0 - c[2]]),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let out = render_with_lut(gpu, &mut exec, &effect, &view, inverted.view(), &params);
    let worst = out
        .as_chunks::<4>()
        .0
        .iter()
        .zip(source.as_chunks::<4>().0.iter())
        .flat_map(|(a, b)| (0..3).map(move |i| (a[i], b[i])))
        .map(|(a, b)| (i32::from(a) + i32::from(b) - 255).unsigned_abs())
        .max()
        .unwrap_or(0);
    assert!(worst <= 3, "inverting LUT is off by {worst}");
}

fn render_with_lut(
    gpu: &Gpu,
    exec: &mut EffectExecutor,
    effect: &CompiledEffect,
    src: &wgpu::TextureView,
    lut: &wgpu::TextureView,
    params: &BTreeMap<String, ParamValue>,
) -> Vec<u8> {
    let (device, queue) = gpu;
    let mut bound = EffectInputs::new();
    bound.bind("src", src).bind("lut", lut);
    let mut encoder = device.create_command_encoder(&Default::default());
    let out = exec
        .run(
            device,
            queue,
            &mut encoder,
            effect,
            &bound,
            params,
            &host(0.0, 0.0, 0),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let row = W as usize * 4;
    let pitch = row.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lut-readback"),
        size: (pitch * H as usize) as u64,
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
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let _ = rx.recv();
    let mapped = slice.get_mapped_range().unwrap_or_else(|e| panic!("{e}"));
    let mut px = vec![0u8; row * H as usize];
    for y in 0..H as usize {
        px[y * row..(y + 1) * row].copy_from_slice(&mapped[y * pitch..y * pitch + row]);
    }
    drop(mapped);
    buffer.unmap();
    exec.recycle(out);
    px
}

#[test]
fn lut_size_outside_the_limit_is_rejected() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    assert!(upload_cube(device, queue, &cube(66, |c| c)).is_err());
    assert!(upload_cube(
        device,
        queue,
        &CubeLut {
            size: 2,
            data: vec![[0.0; 3]; 3]
        }
    )
    .is_err());
}

#[test]
fn two_pass_item_with_a_half_scale_target_reads_prev() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    let json = r#"{
      "id": "half-trip", "version": 1, "kind": "effect", "label": "Half trip",
      "targets": [ { "name": "half", "scale": 0.5, "format": "rgba16f" } ],
      "passes": [
        { "shader": "down.wgsl", "inputs": ["src"], "output": "half" },
        { "shader": "up.wgsl", "inputs": ["prev"], "output": "out" }
      ]
    }"#;
    let shaders = BTreeMap::from([
        (
            "down.wgsl".to_string(),
            "@fragment fn fs_main(in: VsOut) -> @location(0) vec4f { return textureSample(src, samp_linear, in.uv); }".to_string(),
        ),
        (
            "up.wgsl".to_string(),
            "@fragment fn fs_main(in: VsOut) -> @location(0) vec4f { return textureSample(prev, samp_linear, in.uv); }".to_string(),
        ),
    ]);
    let effect =
        CompiledEffect::load(json, &shaders, &BTreeMap::new()).unwrap_or_else(|e| panic!("{e}"));
    let src = upload(device, queue, &gen_bars());
    let view = src.create_view(&Default::default());
    let mut exec = EffectExecutor::new(device);
    let out = render(
        gpu,
        &mut exec,
        &effect,
        &[("src", &view)],
        &BTreeMap::new(),
        &host(0.0, 0.0, 0),
    );
    let source = gen_bars().into_raw();
    let total: u64 = out
        .iter()
        .zip(&source)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    let mean = total as f64 / out.len() as f64;
    eprintln!("half-trip: mean abs diff vs source {mean:.2}");
    assert!(mean < 40.0, "two-pass result drifted too far: {mean}");
}

#[test]
fn missing_inputs_and_bad_params_are_errors() {
    let gpu = gpu_or_skip!();
    let (device, queue) = gpu;
    let blur = load_builtin("blur").unwrap_or_else(|e| panic!("{e}"));
    let mut exec = EffectExecutor::new(device);
    let mut encoder = device.create_command_encoder(&Default::default());
    let no_inputs = EffectInputs::new();
    assert!(exec
        .run(
            device,
            queue,
            &mut encoder,
            &blur,
            &no_inputs,
            &blur_params(1.0),
            &host(0.0, 0.0, 0)
        )
        .is_err());
    let src = upload(device, queue, &gen_bars());
    let view = src.create_view(&Default::default());
    let mut bound = EffectInputs::new();
    bound.bind("src", &view);
    let bad = BTreeMap::from([("radius".to_string(), ParamValue::Vector(vec![1.0, 2.0]))]);
    assert!(exec
        .run(
            device,
            queue,
            &mut encoder,
            &blur,
            &bound,
            &bad,
            &host(0.0, 0.0, 0)
        )
        .is_err());
}
