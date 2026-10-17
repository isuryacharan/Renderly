//! Dual-input transitions (Phase 3), run on the effect runtime.
//!
//! The outgoing and incoming pictures are drawn into two offscreen targets, then the builtin
//! transition item named by the clip's [`TransitionKind`] blends them onto the output (the
//! item's `from` and `to` inputs). Its last pass is alpha-blended straight into the output, so
//! a transition costs no copy pass.

use std::collections::HashMap;

use crate::project::TransitionKind;

use super::effects::ensure_item;
use super::runtime::{CompiledEffect, EffectExecutor, EffectInputs, HostValues, ParamValue};
use super::ComposeError;

struct Offscreen {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

pub struct TransitionPass {
    executor: EffectExecutor,
    /// Compiled builtin transition items, loaded on first use.
    items: HashMap<&'static str, CompiledEffect>,
    rt_a: Option<Offscreen>,
    rt_b: Option<Offscreen>,
}

impl TransitionPass {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            executor: EffectExecutor::new(device),
            items: HashMap::new(),
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

    /// Blends the two offscreen pictures onto `output_view` with the builtin item for `kind`.
    pub fn blend(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output_view: &wgpu::TextureView,
        kind: TransitionKind,
        progress: f32,
    ) -> Result<(), ComposeError> {
        let Self {
            executor,
            items,
            rt_a,
            rt_b,
        } = self;
        let (Some(a), Some(b)) = (rt_a.as_ref(), rt_b.as_ref()) else {
            return Err(ComposeError::Effect(
                "transition targets are not allocated".into(),
            ));
        };
        let item = ensure_item(items, kind.as_str())?;
        let mut inputs = EffectInputs::new();
        inputs.bind("from", &a.view).bind("to", &b.view);
        let host = HostValues {
            width: a.width,
            height: a.height,
            time: 0.0,
            duration: 0.0,
            progress: f64::from(progress.clamp(0.0, 1.0)),
            frame: 0,
            seed: 0,
            speed: 1.0,
        };
        let no_params = |_: &str| -> Option<ParamValue> { None };
        executor
            .run_blended(
                device,
                queue,
                encoder,
                item,
                &inputs,
                &no_params,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::builtins;
    use crate::compose::runtime::manifest::ItemKind;

    /// Every transition kind has a transition item of the same name, and nothing else is a
    /// transition: `TransitionKind::ALL` agrees with the builtin manifests.
    #[test]
    fn transition_kinds_agree_with_builtin_manifests() {
        for kind in TransitionKind::ALL {
            let m = builtins::manifest(kind.as_str())
                .unwrap_or_else(|| panic!("no builtin item for {kind:?}"));
            assert_eq!(m.kind, ItemKind::Transition, "{kind:?}");
        }
        let transitions = builtins::BUILTINS
            .iter()
            .filter(|b| builtins::manifest(b.name).is_some_and(|m| m.kind == ItemKind::Transition))
            .count();
        assert_eq!(transitions, TransitionKind::ALL.len());
    }
}
