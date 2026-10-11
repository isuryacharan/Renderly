//! CPU chroma key → alpha (Phase 4). Production path runs on the GPU in
//! [`super::effects::EffectProcessor`] (improvement-plan A7); this module remains for unit tests.

use crate::frame::RgbaFrame;
use crate::project::EffectInstance;

pub fn apply_chroma_effects(frame: &mut RgbaFrame, effects: &[EffectInstance]) {
    for effect in effects.iter().filter(|e| e.enabled) {
        if effect.effect_id == "builtin:chroma_key" {
            let kr = effect
                .params
                .get("key_r")
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0) as f32;
            let kg = effect
                .params
                .get("key_g")
                .copied()
                .unwrap_or(1.0)
                .clamp(0.0, 1.0) as f32;
            let kb = effect
                .params
                .get("key_b")
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0) as f32;
            let tolerance = effect
                .params
                .get("tolerance")
                .copied()
                .unwrap_or(0.3)
                .clamp(0.0, 1.0) as f32;
            let softness = effect
                .params
                .get("softness")
                .copied()
                .unwrap_or(0.1)
                .clamp(0.0, 1.0) as f32;
            apply_chroma(frame, [kr, kg, kb], tolerance, softness);
        }
    }
}

fn apply_chroma(frame: &mut RgbaFrame, key: [f32; 3], tolerance: f32, softness: f32) {
    let soft = softness.max(1e-4);
    for px in frame.pixels.as_chunks_mut::<4>().0 {
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let dist = ((r - key[0]).powi(2) + (g - key[1]).powi(2) + (b - key[2]).powi(2)).sqrt();
        let alpha = if dist <= tolerance {
            0.0
        } else if dist >= tolerance + soft {
            1.0
        } else {
            (dist - tolerance) / soft
        };
        let src_a = px[3] as f32 / 255.0;
        px[3] = ((src_a * alpha) * 255.0).clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn green_pixels_become_transparent() {
        let mut frame = RgbaFrame {
            width: 2,
            height: 1,
            pixels: vec![0, 255, 0, 255, 255, 0, 0, 255],
        };
        let mut params = BTreeMap::new();
        params.insert("key_r".into(), 0.0);
        params.insert("key_g".into(), 1.0);
        params.insert("key_b".into(), 0.0);
        params.insert("tolerance".into(), 0.2);
        params.insert("softness".into(), 0.05);
        let effect = EffectInstance {
            id: uuid::Uuid::new_v4(),
            effect_id: "builtin:chroma_key".into(),
            enabled: true,
            params,
        };
        apply_chroma_effects(&mut frame, &[effect]);
        assert_eq!(frame.pixels[3], 0);
        assert_eq!(frame.pixels[7], 255);
    }
}
