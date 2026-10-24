//! Dual-input transitions (Phase 3), run on the effect runtime.
//!
//! The outgoing and incoming pictures are drawn into two offscreen targets, then the transition
//! item named by the clip's `ref` (a builtin or a pack item) blends them onto the output (the
//! item's `from` and `to` inputs). Its last pass is alpha-blended straight into the output, so
//! a transition costs no copy pass.

use super::effects::{effect_host, stored_to_runtime};
use super::runtime::manifest::Easing as ItemEasing;
use super::runtime::{CompiledEffect, EffectExecutor, EffectInputs, ParamValue};
use super::{ComposeError, FxTiming, LayerTransition};
use crate::project::ProgressEasing;

struct Offscreen {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

pub struct TransitionPass {
    executor: EffectExecutor,
    rt_a: Option<Offscreen>,
    rt_b: Option<Offscreen>,
}

impl TransitionPass {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            executor: EffectExecutor::new(device),
            rt_a: None,
            rt_b: None,
        }
    }
    /// Marks the start of a submitted frame; see [`EffectExecutor::begin_frame`].
    pub fn begin_frame(&mut self) {
        self.executor.begin_frame();
    }

    pub fn ensure_rts(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let needs = |rt: &Option<Offscreen>| {
            rt.as_ref()
                .map(|r| r.width != width || r.height != height)
                .unwrap_or(true)
        };
        if needs(&self.rt_a) {
            self.rt_a = Some(create_offscreen(device, "trans-a", width, height));
        }
        if needs(&self.rt_b) {
            self.rt_b = Some(create_offscreen(device, "trans-b", width, height));
        }
    }

    pub fn view_a(&self) -> &wgpu::TextureView {
        self.rt_a
            .as_ref()
            .map(|r| &r.view)
            .unwrap_or_else(|| unreachable!("ensure_rts runs before any transition draw"))
    }

    pub fn view_b(&self) -> &wgpu::TextureView {
        self.rt_b
            .as_ref()
            .map(|r| &r.view)
            .unwrap_or_else(|| unreachable!("ensure_rts runs before any transition draw"))
    }

    /// Blends the two offscreen pictures onto `output_view` with the transition `item`, using
    /// the stored `transition.params` and progress (eased by the item's default unless the
    /// transition carries its own easing).
    pub fn blend(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output_view: &wgpu::TextureView,
        item: &CompiledEffect,
        transition: &LayerTransition,
    ) -> Result<(), ComposeError> {
        let Self {
            executor,
            rt_a,
            rt_b,
        } = self;
        let (Some(a), Some(b)) = (rt_a.as_ref(), rt_b.as_ref()) else {
            return Err(ComposeError::Effect(
                "transition targets are not allocated".into(),
            ));
        };
        let mut inputs = EffectInputs::new();
        inputs.bind("from", &a.view).bind("to", &b.view);
        let mut progress = f64::from(transition.progress.clamp(0.0, 1.0));
        if transition.default_easing {
            progress = default_easing(item).apply(progress);
        }
        let mut host = effect_host(a.width, a.height, &FxTiming::default(), 0);
        host.progress = progress;
        let params =
            |key: &str| -> Option<ParamValue> { transition.params.get(key).map(stored_to_runtime) };
        executor
            .run_blended(
                device,
                queue,
                encoder,
                item,
                &inputs,
                &params,
                &host,
                output_view,
            )
            .map_err(|e| ComposeError::Effect(e.to_string()))
    }
}

fn create_offscreen(device: &wgpu::Device, label: &str, width: u32, height: u32) -> Offscreen {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    Offscreen {
        _texture: texture,
        view,
        width,
        height,
    }
}

/// The item's declared default easing as a progress curve (CSS presets for the named ones).
fn default_easing(item: &CompiledEffect) -> ProgressEasing {
    match item.manifest().transition.map(|t| t.easing) {
        Some(ItemEasing::EaseIn) => ProgressEasing::EASE_IN,
        Some(ItemEasing::EaseOut) => ProgressEasing::EASE_OUT,
        Some(ItemEasing::EaseInOut) => ProgressEasing::EASE_IN_OUT,
        _ => ProgressEasing::Linear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::builtins;
    use crate::compose::runtime::manifest::ItemKind;

    /// The ten builtin transitions are exactly the transition items, and each defaults to
    /// linear progress (the goldens depend on it).
    #[test]
    fn builtin_transitions_are_linear_and_listed() {
        let names = builtins::transition_ids();
        assert_eq!(names.len(), 10);
        for name in names {
            let m = builtins::manifest(name).unwrap_or_else(|| panic!("no builtin {name}"));
            assert_eq!(m.kind, ItemKind::Transition, "{name}");
            assert_eq!(
                m.transition.map(|t| t.easing),
                Some(ItemEasing::Linear),
                "{name}"
            );
        }
    }
}
