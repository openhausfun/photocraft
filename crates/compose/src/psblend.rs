//! Photoshop-exact blend functions where they differ from the generic
//! reference in `photocraft_color::blend`, plus the compositing formula.
//!
//! Derived from Photoshop's rendered composites in the corpus oracle
//! (psd-tools `blend-modes/*.psd`):
//!
//! * **Vivid Light**: the source extremes win: `cs = 0` gives 0 even over a
//!   white backdrop, `cs = 1` gives 1 even over black (the generic Color
//!   Burn/Dodge let the backdrop extremes win instead).
//! * **Hard Mix**: the thresholded *generic* vivid light (backdrop extremes
//!   win): 1 where `VL(cb, cs) >= 0.5`, else 0. For interior values this is
//!   the familiar `cb + cs >= 1` rule; the asymmetric edge cases (black
//!   source over white → white, white over black → black) match Photoshop.

use photocraft_color::blend::{self as generic, BlendMode};

fn vivid_light_ps(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        if cs <= 0.0 { 0.0 } else { 1.0 - ((1.0 - cb) / (2.0 * cs)).min(1.0) }
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (2.0 * (1.0 - cs))).min(1.0)
    }
}

/// Backdrop values within `EDGE` of 0 / 1 count as exact in the modes whose result jumps there
/// (Color Burn / Dodge, Hard Mix): a composited value that should be 1 can come out a rounding
/// step below it (and differently on the GPU), flipping the corner case by up to 255 levels.
pub const EDGE: f32 = 1e-4;

fn color_burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 - EDGE {
        1.0
    } else if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

fn color_dodge(cb: f32, cs: f32) -> f32 {
    if cb <= EDGE {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

fn vivid_light_generic(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        if cb >= 1.0 - EDGE {
            1.0
        } else if cs <= 0.0 {
            0.0
        } else {
            1.0 - ((1.0 - cb) / (2.0 * cs)).min(1.0)
        }
    } else if cb <= EDGE {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (2.0 * (1.0 - cs))).min(1.0)
    }
}

fn hard_mix_ps(cb: f32, cs: f32) -> f32 {
    if vivid_light_generic(cb, cs) >= 0.5 - 1e-6 { 1.0 } else { 0.0 }
}

thread_local! {
    /// Set while rendering a Lab document: Normal blending mixes backdrop and source in CIELAB
    /// (Photoshop composites Lab documents in Lab; an anti-aliased edge between two colours
    /// differs by up to 14 / 255 from an sRGB mix: psd-tools stroke-color-descriptors-lab).
    pub static LAB_MIX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `B(Cb, Cs)` with Photoshop's variants.
pub fn blend_rgb(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::VividLight => std::array::from_fn(|i| vivid_light_ps(cb[i], cs[i])),
        BlendMode::HardMix => std::array::from_fn(|i| hard_mix_ps(cb[i], cs[i])),
        BlendMode::ColorBurn => std::array::from_fn(|i| color_burn(cb[i], cs[i])),
        BlendMode::ColorDodge => std::array::from_fn(|i| color_dodge(cb[i], cs[i])),
        m => generic::blend_rgb(m, cb, cs),
    }
}

/// Straight-alpha source over straight-alpha backdrop (W3C/PDF general
/// formula) using [`blend_rgb`].
pub fn composite(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    let ab = backdrop[3];
    let as_ = source[3] * opacity;
    if as_ <= 0.0 {
        return backdrop;
    }
    let ao = as_ + ab * (1.0 - as_);
    if mode == BlendMode::Normal && LAB_MIX.with(|l| l.get()) && ab > 0.0 && ao > 0.0 {
        let lb = photocraft_color::convert::srgb_to_lab([backdrop[0], backdrop[1], backdrop[2]]);
        let ls = photocraft_color::convert::srgb_to_lab([source[0], source[1], source[2]]);
        let kb = ab * (1.0 - as_) / ao;
        let k = as_ / ao;
        let m: [f32; 3] = std::array::from_fn(|i| lb[i] * kb + ls[i] * k);
        let r = photocraft_color::convert::lab_to_srgb(m);
        return [r[0], r[1], r[2], ao];
    }
    if mode == BlendMode::Normal {
        // Fast path: B(Cb, Cs) = Cs.
        if ao <= 0.0 {
            return [0.0; 4];
        }
        let kb = ab * (1.0 - as_);
        let inv = 1.0 / ao;
        return [(kb * backdrop[0] + as_ * source[0]) * inv, (kb * backdrop[1] + as_ * source[1]) * inv, (kb * backdrop[2] + as_ * source[2]) * inv, ao];
    }
    let cb = [backdrop[0], backdrop[1], backdrop[2]];
    let cs = [source[0], source[1], source[2]];
    let b = blend_rgb(mode, cb, cs);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0f32; 4];
    for i in 0..3 {
        out[i] = ((1.0 - as_) * ab * cb[i] + (1.0 - ab) * as_ * cs[i] + as_ * ab * b[i]) / ao;
    }
    out[3] = ao;
    out
}

/// Photoshop's "Blend Text Colors Using Gamma" (Color Settings › Advanced, on at 1.45 by
/// default): type layers mix their colour with the backdrop in linear light raised to 1 / 1.45
/// ([`text_encode`]), so anti-aliased glyph edges come out lighter over dark backdrops than a
/// mix of the encoded values (psd-tools layer_effects.psd: a 69 % edge pixel of a blue overlay
/// over grey is 57/255 in red, an encoded mix gives 40).
pub const TEXT_GAMMA: f32 = 1.45;

static TEXT_GAMMA_SETTING: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3fb9_999a); // 1.45f32

/// Sets Color Settings › "Blend Text Colors Using Gamma" (1 = off). It is an application
/// setting, not stored in documents (1 here means switched off: a plain mix of the encoded
/// values). Values are clamped to Photoshop's 1.00–2.20.
///
/// The value is process-wide: a test that changes it races every concurrently running test
/// that composites a type layer, so such tests belong in their own test binary (see
/// `crates/engine/tests/text_gamma.rs`).
pub fn set_text_gamma(gamma: f32) {
    let g = if gamma.is_finite() { gamma.clamp(1.0, 2.2) } else { TEXT_GAMMA };
    TEXT_GAMMA_SETTING.store(g.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The current text blending gamma (see [`set_text_gamma`]; 1 = off).
pub fn text_gamma() -> f32 {
    f32::from_bits(TEXT_GAMMA_SETTING.load(std::sync::atomic::Ordering::Relaxed))
}

/// Into the text blending space: linear light raised to `1 / gamma`. Gamma 2.2 is close to
/// mixing the encoded values, lower settings approach linear light. Fitted on psd-tools
/// layer_effects.psd (a blue colour overlay's glyph edges over grey, every coverage level within
/// 0.6/255; a plain `v^1.45` power mix is 6/255 too light at 94 % coverage) and ag-psd
/// float-color.
#[inline]
pub fn text_encode(v: f32, gamma: f32) -> f32 {
    photocraft_color::convert::srgb_to_linear(v.max(0.0)).powf(1.0 / gamma)
}

/// Inverse of [`text_encode`].
#[inline]
pub fn text_decode(v: f32, gamma: f32) -> f32 {
    photocraft_color::convert::linear_to_srgb(v.max(0.0).powf(gamma))
}

/// [`composite`] with the coverage mix done in the text blending space of `gamma`
/// ([`text_encode`]; `gamma == 1` is [`composite`], the setting switched off).
pub fn composite_gamma(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32, gamma: f32) -> [f32; 4] {
    if gamma == 1.0 {
        return composite(mode, backdrop, source, opacity);
    }
    let ab = backdrop[3];
    let as_ = source[3] * opacity;
    if as_ <= 0.0 {
        return backdrop;
    }
    let ao = as_ + ab * (1.0 - as_);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let cb = [backdrop[0], backdrop[1], backdrop[2]];
    let cs = [source[0], source[1], source[2]];
    let b = blend_rgb(mode, cb, cs);
    let (wb, ws, wbs) = ((1.0 - as_) * ab / ao, (1.0 - ab) * as_ / ao, as_ * ab / ao);
    let g = |v: f32| text_encode(v, gamma);
    let ginv = |v: f32| text_decode(v, gamma);
    let mut out = [0.0f32; 4];
    for i in 0..3 {
        out[i] = ginv(wb * g(cb[i]) + ws * g(cs[i]) + wbs * g(b[i]));
    }
    out[3] = ao;
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn text_gamma_lightens_dark_edges() {
        // 69 % blue (0, 51, 153) over grey 129: Photoshop shows (57, 80, 146).
        let r = composite_gamma(BlendMode::Normal, [129.0 / 255.0, 129.0 / 255.0, 129.0 / 255.0, 1.0], [0.0, 0.2, 0.6, 0.686], 1.0, TEXT_GAMMA);
        assert!((r[0] * 255.0 - 57.0).abs() < 1.0 && (r[1] * 255.0 - 80.0).abs() < 1.0 && (r[2] * 255.0 - 146.0).abs() < 1.0, "{r:?}");
        // Near-opaque edges stay dark: 94 % coverage shows (13, 57, 152) (a v^1.45 mix gives 19 red).
        let r = composite_gamma(BlendMode::Normal, [129.0 / 255.0, 129.0 / 255.0, 129.0 / 255.0, 1.0], [0.0, 0.2, 0.6, 0.937], 1.0, TEXT_GAMMA);
        assert!((r[0] * 255.0 - 13.0).abs() < 1.0 && (r[1] * 255.0 - 57.0).abs() < 1.0 && (r[2] * 255.0 - 152.0).abs() < 1.0, "{r:?}");
        assert_eq!(
            composite_gamma(BlendMode::Multiply, [0.2, 0.4, 0.6, 0.7], [0.5, 0.1, 0.9, 0.5], 0.8, 1.0),
            composite(BlendMode::Multiply, [0.2, 0.4, 0.6, 0.7], [0.5, 0.1, 0.9, 0.5], 0.8)
        );
    }

    use super::*;

    #[test]
    fn vivid_light_source_extremes_win() {
        assert_eq!(vivid_light_ps(0.0, 1.0), 1.0);
        assert_eq!(vivid_light_ps(1.0, 0.0), 0.0);
        assert!((vivid_light_ps(0.5, 0.75) - 1.0).abs() < 1e-6);
        assert!((vivid_light_ps(0.5, 0.25) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn hard_mix_edges_match_photoshop() {
        assert_eq!(hard_mix_ps(1.0, 0.0), 1.0);
        assert_eq!(hard_mix_ps(0.0, 1.0), 0.0);
        assert_eq!(hard_mix_ps(0.6, 0.6), 1.0);
        assert_eq!(hard_mix_ps(0.3, 0.3), 0.0);
    }

    #[test]
    fn burn_and_dodge_treat_near_extremes_as_exact() {
        assert_eq!(color_burn(1.0 - 1e-6, 0.0), 1.0);
        assert_eq!(color_dodge(1e-6, 1.0), 0.0);
        assert!((color_burn(0.5, 0.5) - 0.0).abs() < 1e-6);
        assert!((color_dodge(0.25, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn other_modes_delegate() {
        for m in BlendMode::LAYER_MODES {
            if matches!(m, BlendMode::VividLight | BlendMode::HardMix | BlendMode::ColorBurn | BlendMode::ColorDodge) {
                continue;
            }
            let (cb, cs) = ([0.2, 0.5, 0.9], [0.7, 0.1, 0.4]);
            assert_eq!(blend_rgb(m, cb, cs), generic::blend_rgb(m, cb, cs));
            let (a, b) =
                (composite(m, [0.2, 0.5, 0.9, 0.7], [0.7, 0.1, 0.4, 0.6], 0.8), generic::composite(m, [0.2, 0.5, 0.9, 0.7], [0.7, 0.1, 0.4, 0.6], 0.8));
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 1e-6, "{m:?} {a:?} {b:?}");
            }
        }
    }
}
