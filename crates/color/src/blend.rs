//! Photoshop blend modes (scalar reference implementation).
//!
//! Formulas follow the public PDF / ISO 32000-2 and W3C Compositing definitions,
//! with Photoshop's variants where they differ (Soft Light, Hard Mix, Darker/Lighter Color,
//! Linear Burn/Dodge, Vivid/Linear/Pin Light, Subtract, Divide). This is the CPU reference
//! that the GPU kernels must match.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlendMode {
    /// Groups only: children blend directly into the group's backdrop.
    PassThrough,
    Normal,
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    /// All modes except PassThrough, in Photoshop menu order.
    pub const LAYER_MODES: [BlendMode; 27] = [
        BlendMode::Normal,
        BlendMode::Dissolve,
        BlendMode::Darken,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::LinearBurn,
        BlendMode::DarkerColor,
        BlendMode::Lighten,
        BlendMode::Screen,
        BlendMode::ColorDodge,
        BlendMode::LinearDodge,
        BlendMode::LighterColor,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BlendMode::PassThrough => "Pass Through",
            BlendMode::Normal => "Normal",
            BlendMode::Dissolve => "Dissolve",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::DarkerColor => "Darker Color",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::LinearDodge => "Linear Dodge (Add)",
            BlendMode::LighterColor => "Lighter Color",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::VividLight => "Vivid Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::PinLight => "Pin Light",
            BlendMode::HardMix => "Hard Mix",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Subtract => "Subtract",
            BlendMode::Divide => "Divide",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// PSD 4-character blend key.
    pub fn psd_key(self) -> [u8; 4] {
        *match self {
            BlendMode::PassThrough => b"pass",
            BlendMode::Normal => b"norm",
            BlendMode::Dissolve => b"diss",
            BlendMode::Darken => b"dark",
            BlendMode::Multiply => b"mul ",
            BlendMode::ColorBurn => b"idiv",
            BlendMode::LinearBurn => b"lbrn",
            BlendMode::DarkerColor => b"dkCl",
            BlendMode::Lighten => b"lite",
            BlendMode::Screen => b"scrn",
            BlendMode::ColorDodge => b"div ",
            BlendMode::LinearDodge => b"lddg",
            BlendMode::LighterColor => b"lgCl",
            BlendMode::Overlay => b"over",
            BlendMode::SoftLight => b"sLit",
            BlendMode::HardLight => b"hLit",
            BlendMode::VividLight => b"vLit",
            BlendMode::LinearLight => b"lLit",
            BlendMode::PinLight => b"pLit",
            BlendMode::HardMix => b"hMix",
            BlendMode::Difference => b"diff",
            BlendMode::Exclusion => b"smud",
            BlendMode::Subtract => b"fsub",
            BlendMode::Divide => b"fdiv",
            BlendMode::Hue => b"hue ",
            BlendMode::Saturation => b"sat ",
            BlendMode::Color => b"colr",
            BlendMode::Luminosity => b"lum ",
        }
    }

    pub fn from_psd_key(key: [u8; 4]) -> Option<BlendMode> {
        std::iter::once(BlendMode::PassThrough).chain(Self::LAYER_MODES).find(|m| m.psd_key() == key)
    }

    pub fn is_separable(self) -> bool {
        !matches!(self, BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity | BlendMode::DarkerColor | BlendMode::LighterColor)
    }
}

/// Separable per-channel blend function `B(cb, cs)`.
#[inline]
pub fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Normal | BlendMode::Dissolve | BlendMode::PassThrough => cs,
        BlendMode::Darken => cb.min(cs),
        BlendMode::Multiply => cb * cs,
        BlendMode::ColorBurn => color_burn(cb, cs),
        BlendMode::LinearBurn => (cb + cs - 1.0).max(0.0),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::ColorDodge => color_dodge(cb, cs),
        BlendMode::LinearDodge => (cb + cs).min(1.0),
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::SoftLight => soft_light_ps(cb, cs),
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::VividLight => {
            if cs <= 0.5 {
                color_burn(cb, 2.0 * cs)
            } else {
                color_dodge(cb, 2.0 * (cs - 0.5))
            }
        }
        BlendMode::LinearLight => (cb + 2.0 * cs - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if cs <= 0.5 {
                cb.min(2.0 * cs)
            } else {
                cb.max(2.0 * cs - 1.0)
            }
        }
        BlendMode::HardMix => {
            if cb + cs >= 1.0 - 1e-6 {
                1.0
            } else {
                0.0
            }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        BlendMode::Subtract => (cb - cs).max(0.0),
        BlendMode::Divide => {
            if cs <= 0.0 {
                if cb <= 0.0 { 0.0 } else { 1.0 }
            } else {
                (cb / cs).min(1.0)
            }
        }
        // Non-separable modes are handled by `blend_rgb`; fall back to source.
        BlendMode::DarkerColor | BlendMode::LighterColor | BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => cs,
    }
}

#[inline]
fn color_burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 {
        1.0
    } else if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

#[inline]
fn color_dodge(cb: f32, cs: f32) -> f32 {
    if cb <= 0.0 {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

#[inline]
fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * 2.0 * cs
    } else {
        let s = 2.0 * cs - 1.0;
        cb + s - cb * s
    }
}

/// Photoshop's Soft Light (differs from the W3C/PDF definition in the upper half).
#[inline]
fn soft_light_ps(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 { 2.0 * cb * cs + cb * cb * (1.0 - 2.0 * cs) } else { 2.0 * cb * (1.0 - cs) + cb.max(0.0).sqrt() * (2.0 * cs - 1.0) }
}

// ---- non-separable helpers (W3C Compositing Level 1) ----

#[inline]
pub fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        let d = l - n;
        for v in &mut out {
            *v = if d.abs() < 1e-12 { l } else { l + (*v - l) * l / d };
        }
    }
    if x > 1.0 {
        let d = x - l;
        for v in &mut out {
            *v = if d.abs() < 1e-12 { l } else { l + (*v - l) * (1.0 - l) / d };
        }
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
    let (imin, imid, imax) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[imax] > c[imin] {
        out[imid] = (c[imid] - c[imin]) * s / (c[imax] - c[imin]);
        out[imax] = s;
    }
    out[imin] = 0.0;
    out
}

/// Blend a whole RGB triple: `B(Cb, Cs)` including non-separable modes.
pub fn blend_rgb(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
        BlendMode::DarkerColor => {
            if lum(cs) < lum(cb) {
                cs
            } else {
                cb
            }
        }
        BlendMode::LighterColor => {
            if lum(cs) > lum(cb) {
                cs
            } else {
                cb
            }
        }
        m => [blend_channel(m, cb[0], cs[0]), blend_channel(m, cb[1], cs[1]), blend_channel(m, cb[2], cs[2])],
    }
}

/// Composite a straight-alpha source over a straight-alpha backdrop with blend mode and opacity.
///
/// Uses the W3C/PDF general formula:
/// `αo = αs + αb(1−αs)`,
/// `αo·Co = (1−αs)·αb·Cb + (1−αb)·αs·Cs + αs·αb·B(Cb,Cs)`.
/// Dissolve must be resolved by the caller (per-pixel coverage decision) before calling.
pub fn composite(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    let ab = backdrop[3];
    let as_ = source[3] * opacity;
    if as_ <= 0.0 {
        return backdrop;
    }
    let cb = [backdrop[0], backdrop[1], backdrop[2]];
    let cs = [source[0], source[1], source[2]];
    let b = blend_rgb(mode, cb, cs);
    let ao = as_ + ab * (1.0 - as_);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0f32; 4];
    for i in 0..3 {
        let premul = (1.0 - as_) * ab * cb[i] + (1.0 - ab) * as_ * cs[i] + as_ * ab * b[i];
        out[i] = premul / ao;
    }
    out[3] = ao;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const E: f32 = 1e-5;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < E
    }

    #[test]
    fn psd_keys_roundtrip_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in std::iter::once(BlendMode::PassThrough).chain(BlendMode::LAYER_MODES) {
            assert!(seen.insert(m.psd_key()), "duplicate key {m:?}");
            assert_eq!(BlendMode::from_psd_key(m.psd_key()), Some(m));
        }
        assert_eq!(BlendMode::from_psd_key(*b"zzzz"), None);
        assert_eq!(seen.len(), 28);
    }

    #[test]
    fn normal_is_source() {
        for &(cb, cs) in &[(0.2, 0.7), (1.0, 0.0), (0.0, 1.0)] {
            assert!(close(blend_channel(BlendMode::Normal, cb, cs), cs));
        }
    }

    #[test]
    fn identities_with_white_and_black() {
        // Multiply by white / screen with black / add black / darken with white leave backdrop unchanged.
        for cb in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
            assert!(close(blend_channel(BlendMode::Multiply, cb, 1.0), cb));
            assert!(close(blend_channel(BlendMode::Screen, cb, 0.0), cb));
            assert!(close(blend_channel(BlendMode::LinearDodge, cb, 0.0), cb));
            assert!(close(blend_channel(BlendMode::Darken, cb, 1.0), cb));
            assert!(close(blend_channel(BlendMode::Lighten, cb, 0.0), cb));
            assert!(close(blend_channel(BlendMode::Difference, cb, 0.0), cb));
            assert!(close(blend_channel(BlendMode::Subtract, cb, 0.0), cb));
            assert!(close(blend_channel(BlendMode::LinearBurn, cb, 1.0), cb));
            assert!(close(blend_channel(BlendMode::Exclusion, cb, 0.0), cb));
            // 50% gray is neutral for the "light" family
            assert!(close(blend_channel(BlendMode::HardLight, cb, 0.5), cb));
            assert!(close(blend_channel(BlendMode::SoftLight, cb, 0.5), cb));
            assert!(close(blend_channel(BlendMode::LinearLight, cb, 0.5), cb));
        }
    }

    #[test]
    fn known_values() {
        assert!(close(blend_channel(BlendMode::Multiply, 0.5, 0.5), 0.25));
        assert!(close(blend_channel(BlendMode::Screen, 0.5, 0.5), 0.75));
        assert!(close(blend_channel(BlendMode::Overlay, 0.25, 0.5), 0.25));
        assert!(close(blend_channel(BlendMode::Overlay, 0.75, 1.0), 1.0));
        assert!(close(blend_channel(BlendMode::ColorDodge, 0.5, 0.5), 1.0));
        assert!(close(blend_channel(BlendMode::ColorBurn, 0.5, 0.5), 0.0));
        assert!(close(blend_channel(BlendMode::ColorBurn, 1.0, 0.0), 1.0));
        assert!(close(blend_channel(BlendMode::ColorDodge, 0.0, 1.0), 0.0));
        assert!(close(blend_channel(BlendMode::Divide, 0.25, 0.5), 0.5));
        assert!(close(blend_channel(BlendMode::Divide, 0.0, 0.0), 0.0));
        assert!(close(blend_channel(BlendMode::Divide, 0.3, 0.0), 1.0));
        assert!(close(blend_channel(BlendMode::Exclusion, 0.5, 0.5), 0.5));
        assert!(close(blend_channel(BlendMode::PinLight, 0.8, 0.2), 0.4));
        assert!(close(blend_channel(BlendMode::PinLight, 0.1, 0.9), 0.8));
        assert!(close(blend_channel(BlendMode::HardMix, 0.6, 0.4), 1.0));
        assert!(close(blend_channel(BlendMode::HardMix, 0.6, 0.3), 0.0));
    }

    #[test]
    fn separable_outputs_stay_in_unit_range() {
        let steps: Vec<f32> = (0..=20).map(|i| i as f32 / 20.0).collect();
        for m in BlendMode::LAYER_MODES {
            for &cb in &steps {
                for &cs in &steps {
                    let v = blend_rgb(m, [cb, cs, 0.5], [cs, cb, 0.25]);
                    for c in v {
                        assert!((-E..=1.0 + E).contains(&c), "{m:?} cb={cb} cs={cs} -> {c}");
                    }
                }
            }
        }
    }

    #[test]
    fn luminosity_and_color_swap_components() {
        let cb = [0.8, 0.2, 0.2];
        let cs = [0.1, 0.1, 0.9];
        let l = blend_rgb(BlendMode::Luminosity, cb, cs);
        assert!(close(lum(l), lum(cs)));
        let c = blend_rgb(BlendMode::Color, cb, cs);
        assert!(close(lum(c), lum(cb)));
        let h = blend_rgb(BlendMode::Hue, cb, cs);
        assert!(close(lum(h), lum(cb)));
        let s = blend_rgb(BlendMode::Saturation, cb, [0.5, 0.5, 0.5]);
        // zero-saturation source desaturates the backdrop
        assert!(close(s[0], s[1]) && close(s[1], s[2]));
    }

    #[test]
    fn darker_lighter_color_pick_whole_pixel() {
        let a = [0.9, 0.1, 0.1];
        let b = [0.2, 0.6, 0.2];
        assert_eq!(blend_rgb(BlendMode::DarkerColor, a, b), if lum(b) < lum(a) { b } else { a });
        assert_eq!(blend_rgb(BlendMode::LighterColor, a, b), if lum(b) > lum(a) { b } else { a });
    }

    #[test]
    fn composite_alpha_rules() {
        let bg = [0.2, 0.4, 0.6, 1.0];
        // Transparent source leaves backdrop.
        assert_eq!(composite(BlendMode::Multiply, bg, [1.0, 0.0, 0.0, 0.0], 1.0), bg);
        // Opaque normal source replaces.
        let r = composite(BlendMode::Normal, bg, [1.0, 0.0, 0.0, 1.0], 1.0);
        assert!(close(r[0], 1.0) && close(r[3], 1.0));
        // Half opacity normal = lerp.
        let r = composite(BlendMode::Normal, bg, [1.0, 1.0, 1.0, 1.0], 0.5);
        assert!(close(r[0], 0.6) && close(r[1], 0.7) && close(r[2], 0.8));
        // Blend over transparent backdrop yields source regardless of mode.
        let r = composite(BlendMode::Multiply, [0.0; 4], [0.3, 0.5, 0.7, 1.0], 1.0);
        assert!(close(r[0], 0.3) && close(r[1], 0.5) && close(r[2], 0.7) && close(r[3], 1.0));
        // Alpha accumulates.
        let r = composite(BlendMode::Screen, [0.0, 0.0, 0.0, 0.5], [0.0, 0.0, 0.0, 0.5], 1.0);
        assert!(close(r[3], 0.75));
    }
}
