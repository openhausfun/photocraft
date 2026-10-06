//! Full parameter sets for the per-pixel adjustment kinds, shared by the destructive
//! `image.adjustments.*` commands, `layer.newAdjustmentLayer.*` and `layer.setAdjustment`.
//!
//! [`from_params`] reads params over a base adjustment (missing keys keep the base's value, so a
//! partial update only changes what it names) and [`to_params`] writes the complete set back, so
//! `from_params(kind, &to_params(a), None, mode) == a`. Values out of range are clamped like
//! Photoshop's fields; a key of the wrong type is an error.
//!
//! Units: Levels and Curves in 0–255 levels; percentages for the rest (as in Photoshop's dialogs);
//! colours as `"#rrggbb"` or `[r, g, b]` in 0–1.

use photocraft_color::ColorMode;
use photocraft_doc::Adjustment;
use photocraft_doc::adjust::{CurvePoint, HueRange, LevelsChannel, ToneSpace};
use serde_json::{Map, Value, json};

use crate::{EngineError, Result};

/// Hue/Saturation and Black & White colour-range keys (storage order).
pub const HUE_RANGES: [&str; 6] = ["reds", "yellows", "greens", "cyans", "blues", "magentas"];

/// Black & White default weights (percent) for [`HUE_RANGES`].
pub const BW_DEFAULTS: [f32; 6] = [40.0, 60.0, 40.0, 60.0, 20.0, 80.0];

/// Photo Filter presets `(id, label, colour)`: common photographic filter colours.
pub const PHOTO_FILTERS: &[(&str, &str, [u8; 3])] = &[
    ("warming85", "Warming Filter (85)", [236, 138, 0]),
    ("warmingLBA", "Warming Filter (LBA)", [250, 150, 0]),
    ("warming81", "Warming Filter (81)", [235, 177, 19]),
    ("cooling80", "Cooling Filter (80)", [0, 109, 255]),
    ("coolingLBB", "Cooling Filter (LBB)", [0, 93, 255]),
    ("cooling82", "Cooling Filter (82)", [0, 181, 255]),
    ("red", "Red", [234, 26, 26]),
    ("orange", "Orange", [243, 132, 23]),
    ("yellow", "Yellow", [249, 227, 28]),
    ("green", "Green", [25, 201, 25]),
    ("cyan", "Cyan", [29, 203, 234]),
    ("blue", "Blue", [29, 53, 234]),
    ("violet", "Violet", [155, 29, 234]),
    ("magenta", "Magenta", [227, 24, 227]),
    ("sepia", "Sepia", [172, 122, 51]),
    ("deepRed", "Deep Red", [255, 0, 0]),
    ("deepBlue", "Deep Blue", [0, 34, 205]),
    ("deepEmerald", "Deep Emerald", [0, 140, 0]),
    ("deepYellow", "Deep Yellow", [255, 213, 0]),
    ("underwater", "Underwater", [0, 194, 177]),
];

/// Per-channel keys of Levels/Curves in each tone space (after the composite).
pub fn channel_keys(space: ToneSpace) -> &'static [&'static str] {
    match space {
        ToneSpace::Rgb => &["red", "green", "blue"],
        ToneSpace::Cmyk => &["cyan", "magenta", "yellow", "black"],
        ToneSpace::Lab => &["lightness", "a", "b"],
    }
}

/// The tone space new Levels/Curves get in a document of `mode`.
pub fn default_space(mode: ColorMode) -> ToneSpace {
    match mode {
        ColorMode::Cmyk => ToneSpace::Cmyk,
        ColorMode::Lab => ToneSpace::Lab,
        _ => ToneSpace::Rgb,
    }
}

/// The neutral adjustment of `kind` (what "Reset to defaults" restores).
pub fn default_for(kind: &str, mode: ColorMode) -> Result<Adjustment> {
    from_params(kind, &json!({}), None, mode)
}

/// Reads the adjustment of `kind` from `p` over `base` (or the kind's defaults when `base` is
/// None or of another kind). `mode` picks the Levels/Curves channel space when neither the params
/// nor the base name one.
pub fn from_params(kind: &str, p: &Value, base: Option<&Adjustment>, mode: ColorMode) -> Result<Adjustment> {
    let cmd = format!("adjustment `{kind}`");
    if !p.is_object() && !p.is_null() {
        return Err(bad(&cmd, "params must be an object"));
    }
    let r = Reader { cmd: &cmd, p };
    Ok(match kind {
        "invert" => Adjustment::Invert,
        "brightnessContrast" => {
            let (b0, c0, l0) = match base {
                Some(Adjustment::BrightnessContrast { brightness, contrast, legacy }) => (*brightness, *contrast, *legacy),
                _ => (0.0, 0.0, false),
            };
            let legacy = r.boolean("legacy", l0)?;
            let (lo, hi) = if legacy { (-100.0, 100.0) } else { (-50.0, 100.0) };
            Adjustment::BrightnessContrast { brightness: r.num("brightness", b0, -150.0, 150.0)?, contrast: r.num("contrast", c0, lo, hi)?, legacy }
        }
        "threshold" => {
            let l0 = match base {
                Some(Adjustment::Threshold { level }) => level * 255.0,
                _ => 128.0,
            };
            Adjustment::Threshold { level: r.num("level", l0, 1.0, 255.0)?.round() / 255.0 }
        }
        "posterize" => {
            let l0 = match base {
                Some(Adjustment::Posterize { levels }) => *levels as f32,
                _ => 4.0,
            };
            Adjustment::Posterize { levels: r.num("levels", l0, 2.0, 255.0)?.round() as u32 }
        }
        "exposure" => {
            let (e, o, g) = match base {
                Some(Adjustment::Exposure { exposure, offset, gamma }) => (*exposure, *offset, *gamma),
                _ => (0.0, 0.0, 1.0),
            };
            Adjustment::Exposure { exposure: r.num("exposure", e, -20.0, 20.0)?, offset: r.num("offset", o, -0.5, 0.5)?, gamma: r.num("gamma", g, 0.01, 9.99)? }
        }
        "vibrance" => {
            let (v, s) = match base {
                Some(Adjustment::Vibrance { vibrance, saturation }) => (*vibrance, *saturation),
                _ => (0.0, 0.0),
            };
            Adjustment::Vibrance { vibrance: r.num("vibrance", v, -100.0, 100.0)?, saturation: r.num("saturation", s, -100.0, 100.0)? }
        }
        "hueSaturation" => hue_saturation(&r, base)?,
        "levels" => levels(&r, base, mode)?,
        "curves" => curves(&r, base, mode)?,
        "colorBalance" => {
            let (mut tones, preserve) = match base {
                Some(Adjustment::ColorBalance { shadows, midtones, highlights, preserve_luminosity }) => {
                    ([*shadows, *midtones, *highlights], *preserve_luminosity)
                }
                _ => ([[0.0; 3]; 3], true),
            };
            for (i, key) in ["shadows", "midtones", "highlights"].iter().enumerate() {
                if let Some(v) = r.numbers(key, 3)? {
                    tones[i] = [v[0], v[1], v[2]].map(|x| x.clamp(-100.0, 100.0));
                }
            }
            Adjustment::ColorBalance {
                shadows: tones[0],
                midtones: tones[1],
                highlights: tones[2],
                preserve_luminosity: r.boolean("preserveLuminosity", preserve)?,
            }
        }
        "blackWhite" => {
            let (mut weights, tint0) = match base {
                Some(Adjustment::BlackWhite { weights, tint }) => (*weights, *tint),
                _ => (BW_DEFAULTS, None),
            };
            for (i, key) in HUE_RANGES.iter().enumerate() {
                weights[i] = r.num(key, weights[i], -200.0, 300.0)?;
            }
            // Photoshop's default tint colour is a light sepia.
            let color = r.color("tintColor", tint0.unwrap_or([0.882, 0.827, 0.702]))?;
            let tint = r.boolean("tint", tint0.is_some())?.then_some(color);
            Adjustment::BlackWhite { weights, tint }
        }
        "photoFilter" => {
            let (c0, d0, p0) = match base {
                Some(Adjustment::PhotoFilter { color, density, preserve_luminosity }) => (*color, *density, *preserve_luminosity),
                _ => ([236.0 / 255.0, 138.0 / 255.0, 0.0], 0.25, true),
            };
            let preset = match p.get("filter") {
                None | Some(Value::Null) => None,
                Some(Value::String(id)) => {
                    let f = PHOTO_FILTERS.iter().find(|f| f.0 == id).ok_or_else(|| bad(&cmd, format!("unknown filter `{id}`")))?;
                    Some(f.2.map(|v| f32::from(v) / 255.0))
                }
                Some(_) => return Err(bad(&cmd, "`filter` must be a preset id")),
            };
            let color = r.color("color", preset.unwrap_or(c0))?;
            Adjustment::PhotoFilter {
                color,
                density: r.num("density", d0 * 100.0, 0.0, 100.0)? / 100.0,
                preserve_luminosity: r.boolean("preserveLuminosity", p0)?,
            }
        }
        "channelMixer" => {
            let (mut matrix, mono0) = match base {
                Some(Adjustment::ChannelMixer { matrix, monochrome }) => (*matrix, *monochrome),
                _ => ([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], false),
            };
            let monochrome = r.boolean("monochrome", mono0)?;
            if monochrome && !mono0 && p.get("gray").is_none() {
                // Turning Monochrome on starts from Photoshop's grey mix.
                matrix[0] = [0.4, 0.4, 0.2, 0.0];
            }
            for (i, key) in ["red", "green", "blue"].iter().enumerate() {
                if let Some(v) = r.numbers(key, 4)? {
                    matrix[i] = mixer_row(&v);
                }
            }
            if let Some(v) = r.numbers("gray", 4)? {
                matrix[0] = mixer_row(&v);
            }
            Adjustment::ChannelMixer { matrix, monochrome }
        }
        "gradientMap" => {
            let (stops0, rev0, dither0) = match base {
                Some(Adjustment::GradientMap { stops, reverse, dither }) => (stops.clone(), *reverse, *dither),
                _ => (vec![(0.0, [0.0; 3]), (1.0, [1.0; 3])], false, false),
            };
            let stops = match p.get("stops") {
                None | Some(Value::Null) => stops0,
                Some(v) => gradient_stops(&cmd, v)?,
            };
            Adjustment::GradientMap { stops, reverse: r.boolean("reverse", rev0)?, dither: r.boolean("dither", dither0)? }
        }
        "selectiveColor" => crate::adjust_cmds::selective_from_params(p, base.filter(|b| matches!(b, Adjustment::SelectiveColor { .. }))),
        "colorLookup" => crate::adjust_cmds::lookup_from_params(p, base.filter(|b| matches!(b, Adjustment::ColorLookup { .. })))?,
        other => return Err(bad("adjustment", format!("unknown adjustment kind `{other}`"))),
    })
}

/// The complete parameter set of `adj` (the inverse of [`from_params`]).
pub fn to_params(adj: &Adjustment) -> Value {
    match adj {
        Adjustment::Invert | Adjustment::Unsupported { .. } => json!({}),
        Adjustment::BrightnessContrast { brightness, contrast, legacy } => json!({"brightness": brightness, "contrast": contrast, "legacy": legacy}),
        Adjustment::Threshold { level } => json!({"level": (level * 255.0).round()}),
        Adjustment::Posterize { levels } => json!({"levels": levels}),
        Adjustment::Exposure { exposure, offset, gamma } => json!({"exposure": exposure, "offset": offset, "gamma": gamma}),
        Adjustment::Vibrance { vibrance, saturation } => json!({"vibrance": vibrance, "saturation": saturation}),
        Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges } => {
            let mut v = json!({"hue": hue, "saturation": saturation, "lightness": lightness, "colorize": colorize});
            for (key, r) in HUE_RANGES.iter().zip(ranges.iter()) {
                v[*key] = json!({"hue": r.hue, "saturation": r.saturation, "lightness": r.lightness, "range": r.bounds});
            }
            v
        }
        Adjustment::Levels { master, per_channel, space, black } => {
            let mut v = levels_obj(master);
            for (key, c) in channel_keys(*space).iter().zip([&per_channel[0], &per_channel[1], &per_channel[2], black]) {
                v[*key] = levels_obj(c);
            }
            if *space == ToneSpace::Lab {
                // No composite in Lab: the top-level keys are the lightness channel.
                v = merge(levels_obj(&per_channel[0]), v);
            }
            v
        }
        Adjustment::Curves { master, per_channel, space, black } => {
            let mut v = json!({"points": curve_arr(master)});
            let ink = if black.len() >= 2 { black.clone() } else { identity_curve() };
            let chans = per_channel.iter().cloned().chain(std::iter::once(ink));
            for (key, c) in channel_keys(*space).iter().zip(chans) {
                v[*key] = curve_arr(&c);
            }
            if *space == ToneSpace::Lab {
                v["points"] = curve_arr(&per_channel[0]);
            }
            v
        }
        Adjustment::ColorBalance { shadows, midtones, highlights, preserve_luminosity } => {
            json!({"shadows": shadows, "midtones": midtones, "highlights": highlights, "preserveLuminosity": preserve_luminosity})
        }
        Adjustment::BlackWhite { weights, tint } => {
            let mut v = json!({"tint": tint.is_some(), "tintColor": hex(tint.unwrap_or([0.882, 0.827, 0.702]))});
            for (key, w) in HUE_RANGES.iter().zip(weights.iter()) {
                v[*key] = json!(w);
            }
            v
        }
        Adjustment::PhotoFilter { color, density, preserve_luminosity } => {
            json!({"color": hex(*color), "density": (density * 1000.0).round() / 10.0, "preserveLuminosity": preserve_luminosity})
        }
        Adjustment::ChannelMixer { matrix, monochrome } => {
            let row = |r: &[f32; 4]| json!(r.map(|x| (x * 1000.0).round() / 10.0));
            let mut v = json!({"red": row(&matrix[0]), "green": row(&matrix[1]), "blue": row(&matrix[2]), "monochrome": monochrome});
            if *monochrome {
                v["gray"] = row(&matrix[0]);
            }
            v
        }
        Adjustment::GradientMap { stops, reverse, dither } => {
            json!({"stops": stops.iter().map(|(t, c)| json!([t, hex(*c)])).collect::<Vec<_>>(), "reverse": reverse, "dither": dither})
        }
        Adjustment::SelectiveColor { relative, adjustments } => {
            let mut v = json!({"method": if *relative { "relative" } else { "absolute" }});
            for (i, key) in crate::adjust_cmds::RANGES.iter().enumerate() {
                v[*key] = json!(adjustments[i]);
            }
            v
        }
        Adjustment::ColorLookup { tetrahedral, dither, .. } => {
            json!({"interpolation": if *tetrahedral { "tetrahedral" } else { "trilinear" }, "dither": dither})
        }
    }
}

// ---------------------------------------------------------------------------------------------

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// Typed, clamped access to optional params.
struct Reader<'a> {
    cmd: &'a str,
    p: &'a Value,
}

impl Reader<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.p.get(key).filter(|v| !v.is_null())
    }

    fn num(&self, key: &str, default: f32, lo: f32, hi: f32) -> Result<f32> {
        match self.get(key) {
            None => Ok(default.clamp(lo, hi)),
            Some(v) => num_in(self.cmd, key, v, lo, hi),
        }
    }

    fn boolean(&self, key: &str, default: bool) -> Result<bool> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(bad(self.cmd, format!("`{key}` must be true or false"))),
        }
    }

    /// An array of exactly `n` numbers.
    fn numbers(&self, key: &str, n: usize) -> Result<Option<Vec<f32>>> {
        let Some(v) = self.get(key) else { return Ok(None) };
        let a = v.as_array().filter(|a| a.len() == n).ok_or_else(|| bad(self.cmd, format!("`{key}` must be an array of {n} numbers")))?;
        a.iter().map(|x| num_in(self.cmd, key, x, -1e6, 1e6)).collect::<Result<Vec<_>>>().map(Some)
    }

    fn color(&self, key: &str, default: [f32; 3]) -> Result<[f32; 3]> {
        match self.get(key) {
            None => Ok(default),
            Some(v) => color_of(self.cmd, key, v),
        }
    }

    /// A channel object (`{"inBlack": …}`) or None.
    fn object(&self, key: &str) -> Result<Option<Reader<'_>>> {
        match self.get(key) {
            None => Ok(None),
            Some(v) if v.is_object() => Ok(Some(Reader { cmd: self.cmd, p: v })),
            Some(_) => Err(bad(self.cmd, format!("`{key}` must be an object"))),
        }
    }
}

fn num_in(cmd: &str, key: &str, v: &Value, lo: f32, hi: f32) -> Result<f32> {
    let x = v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(cmd, format!("`{key}` must be a number")))?;
    Ok((x as f32).clamp(lo, hi))
}

fn color_of(cmd: &str, key: &str, v: &Value) -> Result<[f32; 3]> {
    let err = || bad(cmd, format!("`{key}` must be \"#rrggbb\" or [r, g, b] in 0..1"));
    match v {
        Value::String(s) => {
            let h = s.trim_start_matches('#');
            let b = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).map(|x| f32::from(x) / 255.0);
            match (h.len(), b(0), b(2), b(4)) {
                (6 | 8, Some(r), Some(g), Some(bb)) => Ok([r, g, bb]),
                _ => Err(err()),
            }
        }
        Value::Array(a) if a.len() >= 3 => {
            let c = |i: usize| a.get(i).and_then(Value::as_f64).filter(|x| x.is_finite()).map(|x| (x as f32).clamp(0.0, 1.0)).ok_or_else(err);
            Ok([c(0)?, c(1)?, c(2)?])
        }
        _ => Err(err()),
    }
}

pub fn hex(c: [f32; 3]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(ao), Value::Object(bo)) = (a.as_object_mut(), b) {
        for (k, v) in bo {
            ao.entry(k).or_insert(v);
        }
    }
    a
}

fn mixer_row(v: &[f32]) -> [f32; 4] {
    std::array::from_fn(|i| v.get(i).copied().unwrap_or(0.0).clamp(-200.0, 200.0) / 100.0)
}

fn hue_saturation(r: &Reader<'_>, base: Option<&Adjustment>) -> Result<Adjustment> {
    let (h, s, l, c, mut ranges) = match base {
        Some(Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges }) => (*hue, *saturation, *lightness, *colorize, ranges.clone()),
        _ => (0.0, 0.0, 0.0, false, HueRange::defaults()),
    };
    let colorize = r.boolean("colorize", c)?;
    // Colorize hue is 0..360, a shift is -180..180.
    let hue = if colorize { r.num("hue", h.rem_euclid(360.0), 0.0, 360.0)? } else { r.num("hue", h, -180.0, 180.0)? };
    let (saturation, lightness) = (r.num("saturation", s, if colorize { 0.0 } else { -100.0 }, 100.0)?, r.num("lightness", l, -100.0, 100.0)?);
    for (i, key) in HUE_RANGES.iter().enumerate() {
        let Some(o) = r.object(key)? else { continue };
        let Some(range) = ranges.get_mut(i) else { continue };
        range.hue = o.num("hue", range.hue, -180.0, 180.0)?;
        range.saturation = o.num("saturation", range.saturation, -100.0, 100.0)?;
        range.lightness = o.num("lightness", range.lightness, -100.0, 100.0)?;
        if let Some(b) = o.numbers("range", 4)? {
            // Keep the four sliders ordered and within one turn of the first.
            range.bounds = HueRange::canonical_bounds([b[0], b[1], b[2], b[3]]);
        }
    }
    Ok(Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges })
}

/// Which tone space the params address: any channel key of a space picks it.
fn space_of(r: &Reader<'_>, base: Option<ToneSpace>, mode: ColorMode) -> ToneSpace {
    let has = |keys: &[&str]| keys.iter().any(|k| r.get(k).is_some());
    if has(&["cyan", "magenta", "yellow", "black"]) {
        ToneSpace::Cmyk
    } else if has(&["a", "b"]) || (has(&["lightness"]) && !has(&["red", "green", "blue"])) {
        ToneSpace::Lab
    } else if has(&["red", "green", "blue"]) {
        ToneSpace::Rgb
    } else {
        base.unwrap_or_else(|| default_space(mode))
    }
}

fn levels(r: &Reader<'_>, base: Option<&Adjustment>, mode: ColorMode) -> Result<Adjustment> {
    let (mut master, mut chans, base_space) = match base {
        Some(Adjustment::Levels { master, per_channel, space, black }) => {
            (master.clone(), [per_channel[0].clone(), per_channel[1].clone(), per_channel[2].clone(), black.clone()], Some(*space))
        }
        _ => (LevelsChannel::default(), Default::default(), None),
    };
    let space = space_of(r, base_space, mode);
    if base_space.is_some_and(|b| b != space) {
        chans = Default::default();
    }
    let read = |o: &Reader<'_>, c: &LevelsChannel| -> Result<LevelsChannel> {
        let in_black = o.num("inBlack", c.in_black * 255.0, 0.0, 253.0)?;
        let in_white = o.num("inWhite", c.in_white * 255.0, 2.0, 255.0)?.max(in_black + 2.0);
        Ok(LevelsChannel {
            in_black: in_black / 255.0,
            in_white: in_white / 255.0,
            gamma: o.num("gamma", c.gamma, 0.01, 9.99)?,
            out_black: o.num("outBlack", c.out_black * 255.0, 0.0, 255.0)? / 255.0,
            out_white: o.num("outWhite", c.out_white * 255.0, 0.0, 255.0)? / 255.0,
        })
    };
    if space == ToneSpace::Lab {
        // Top-level keys (and "gray") address lightness: Lab has no composite.
        chans[0] = read(r, &chans[0])?;
    } else {
        master = read(r, &master)?;
        if let Some(o) = r.object("gray")? {
            master = read(&o, &master)?;
        }
    }
    for (i, key) in channel_keys(space).iter().enumerate() {
        if let (Some(o), Some(c)) = (r.object(key)?, chans.get(i).cloned()) {
            chans[i] = read(&o, &c)?;
        }
    }
    let [c0, c1, c2, black] = chans;
    Ok(Adjustment::Levels { master, per_channel: [c0, c1, c2], space, black: if space == ToneSpace::Cmyk { black } else { LevelsChannel::default() } })
}

fn identity_curve() -> Vec<CurvePoint> {
    vec![CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }]
}

fn curve_arr(c: &[CurvePoint]) -> Value {
    let c = if c.len() >= 2 { c.to_vec() } else { identity_curve() };
    json!(c.iter().map(|p| [(p.input * 255.0 * 100.0).round() / 100.0, (p.output * 255.0 * 100.0).round() / 100.0]).collect::<Vec<_>>())
}

/// Most points a curve may have (Photoshop allows 16; the PSD record holds 19).
pub const MAX_CURVE_POINTS: usize = 19;

/// `[[in, out], …]` in 0..255: sorted by input, duplicates collapsed, 2..=19 points.
pub fn parse_curve(cmd: &str, key: &str, v: &Value) -> Result<Vec<CurvePoint>> {
    let err = || bad(cmd, format!("`{key}` must be an array of 2..={MAX_CURVE_POINTS} [input, output] pairs in 0..255"));
    let a = v.as_array().ok_or_else(err)?;
    if a.len() < 2 || a.len() > 64 {
        return Err(err());
    }
    let mut pts = Vec::with_capacity(a.len());
    for q in a {
        let q = q.as_array().filter(|q| q.len() == 2).ok_or_else(err)?;
        let x = q.first().and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(err)?;
        let y = q.get(1).and_then(Value::as_f64).filter(|y| y.is_finite()).ok_or_else(err)?;
        pts.push(CurvePoint { input: (x as f32).clamp(0.0, 255.0) / 255.0, output: (y as f32).clamp(0.0, 255.0) / 255.0 });
    }
    pts.sort_by(|a, b| a.input.total_cmp(&b.input));
    pts.dedup_by(|later, earlier| (later.input - earlier.input).abs() < 0.5 / 255.0);
    if pts.len() < 2 || pts.len() > MAX_CURVE_POINTS {
        return Err(err());
    }
    Ok(pts)
}

fn curves(r: &Reader<'_>, base: Option<&Adjustment>, mode: ColorMode) -> Result<Adjustment> {
    let (mut master, mut chans, base_space) = match base {
        Some(Adjustment::Curves { master, per_channel, space, black }) => {
            (master.clone(), [per_channel[0].clone(), per_channel[1].clone(), per_channel[2].clone(), black.clone()], Some(*space))
        }
        _ => (identity_curve(), std::array::from_fn(|_| identity_curve()), None),
    };
    let space = space_of(r, base_space, mode);
    if base_space.is_some_and(|b| b != space) {
        chans = std::array::from_fn(|_| identity_curve());
    }
    let composite = r.get("points").or_else(|| r.get("gray"));
    if let Some(v) = composite {
        let c = parse_curve(r.cmd, "points", v)?;
        if space == ToneSpace::Lab {
            chans[0] = c;
        } else {
            master = c;
        }
    }
    for (i, key) in channel_keys(space).iter().enumerate() {
        if let Some(v) = r.get(key)
            && let Some(slot) = chans.get_mut(i)
        {
            *slot = parse_curve(r.cmd, key, v)?;
        }
    }
    let [c0, c1, c2, black] = chans;
    let black = if space == ToneSpace::Cmyk && !photocraft_doc::adjust::is_identity_curve(&black) { black } else { Vec::new() };
    Ok(Adjustment::Curves { master, per_channel: [c0, c1, c2], space, black })
}

fn levels_obj(c: &LevelsChannel) -> Value {
    let q = |v: f32| (v * 255.0 * 100.0).round() / 100.0;
    json!({"inBlack": q(c.in_black), "gamma": (c.gamma * 1000.0).round() / 1000.0, "inWhite": q(c.in_white), "outBlack": q(c.out_black), "outWhite": q(c.out_white)})
}

/// `[[location 0..1, colour], …]` (colour as for `color`), sorted by location.
fn gradient_stops(cmd: &str, v: &Value) -> Result<Vec<(f32, [f32; 3])>> {
    let err = || bad(cmd, "`stops` must be an array of 2..=64 [location 0..1, colour] pairs");
    let a = v.as_array().filter(|a| (2..=64).contains(&a.len())).ok_or_else(err)?;
    let mut out = Vec::with_capacity(a.len());
    for s in a {
        let (t, c) = match s {
            Value::Array(p) if p.len() == 2 => (p.first(), p.get(1)),
            Value::Object(o) => (o.get("location"), o.get("color")),
            _ => return Err(err()),
        };
        let t = t.and_then(Value::as_f64).filter(|t| t.is_finite()).ok_or_else(err)?;
        let c = color_of(cmd, "stops", c.ok_or_else(err)?)?;
        out.push(((t as f32).clamp(0.0, 1.0), c));
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(out)
}

/// Params with the private keys (`layer`, `coalesce`, `__kind`) removed.
pub fn user_keys(p: &Value) -> Map<String, Value> {
    p.as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "layer" | "coalesce" | "channel" | "targetChannel") && !k.starts_with("__"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [&str; 15] = [
        "invert",
        "brightnessContrast",
        "threshold",
        "posterize",
        "exposure",
        "vibrance",
        "hueSaturation",
        "levels",
        "curves",
        "colorBalance",
        "blackWhite",
        "photoFilter",
        "channelMixer",
        "gradientMap",
        "selectiveColor",
    ];

    /// A non-default parameter set per kind (RGB channel keys for Levels/Curves).
    pub(crate) fn sample(kind: &str) -> Value {
        match kind {
            "brightnessContrast" => json!({"brightness": 20, "contrast": -10, "legacy": true}),
            "threshold" => json!({"level": 100}),
            "posterize" => json!({"levels": 7}),
            "exposure" => json!({"exposure": 1.5, "offset": 0.1, "gamma": 1.4}),
            "vibrance" => json!({"vibrance": 30, "saturation": -20}),
            "hueSaturation" => {
                json!({"hue": 25, "saturation": -20, "lightness": 10, "reds": {"hue": 10, "saturation": -30, "lightness": 5, "range": [300, 340, 20, 50]}})
            }
            "levels" => json!({"inBlack": 10, "inWhite": 240, "outBlack": 5, "outWhite": 250, "gamma": 1.2, "red": {"inBlack": 3, "gamma": 0.8}}),
            "curves" => json!({"points": [[0, 0], [128, 150], [255, 255]], "blue": [[0, 20], [255, 235]]}),
            "colorBalance" => json!({"shadows": [10, -5, 0], "midtones": [0, 20, -30], "highlights": [5, 5, 5], "preserveLuminosity": false}),
            "blackWhite" => json!({"reds": 120, "blues": -40, "tint": true, "tintColor": "#d0a070"}),
            "photoFilter" => json!({"color": "#3366cc", "density": 40, "preserveLuminosity": false}),
            "channelMixer" => json!({"red": [80, 30, -10, 5], "blue": [0, 10, 90, 0]}),
            "gradientMap" => json!({"stops": [[0, "#000000"], [0.5, "#ff0000"], [1, [1, 1, 1]]], "reverse": true, "dither": true}),
            "selectiveColor" => json!({"method": "absolute", "reds": [10, 0, -20, 5]}),
            _ => json!({}),
        }
    }

    #[test]
    fn every_kind_round_trips_through_params() {
        for mode in [ColorMode::Rgb, ColorMode::Cmyk, ColorMode::Lab, ColorMode::Grayscale] {
            for kind in KINDS {
                let mut p = sample(kind);
                if matches!(kind, "levels" | "curves") && default_space(mode) != ToneSpace::Rgb {
                    // Off RGB the channel keys are the document's.
                    p.as_object_mut().unwrap().retain(|k, _| !matches!(k.as_str(), "red" | "blue"));
                }
                let a = from_params(kind, &p, None, mode).unwrap_or_else(|e| panic!("{kind} {mode:?}: {e}"));
                assert_ne!(Some(&a), default_for(kind, mode).ok().as_ref().filter(|_| kind != "invert"), "{kind} params ignored");
                let back = from_params(kind, &to_params(&a), None, mode).unwrap();
                assert_eq!(to_params(&back), to_params(&a), "{kind} {mode:?}");
                // A partial update keeps everything it doesn't name.
                let again = from_params(kind, &json!({}), Some(&a), mode).unwrap();
                assert_eq!(again, a, "{kind} {mode:?}");
            }
        }
    }

    #[test]
    fn params_reach_the_model() {
        let a = from_params("colorBalance", &json!({"midtones": [50, 0, -200]}), None, ColorMode::Rgb).unwrap();
        assert_eq!(a, Adjustment::ColorBalance { shadows: [0.0; 3], midtones: [50.0, 0.0, -100.0], highlights: [0.0; 3], preserve_luminosity: true });
        let a = from_params("channelMixer", &json!({"monochrome": true}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(a, Adjustment::ChannelMixer { matrix, monochrome: true } if matrix[0] == [0.4, 0.4, 0.2, 0.0]));
        let a = from_params("photoFilter", &json!({"filter": "cooling80", "density": 60}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(a, Adjustment::PhotoFilter { color, density, .. } if color[2] == 1.0 && (density - 0.6).abs() < 1e-6));
        let a = from_params("blackWhite", &json!({"reds": 120, "tint": true}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(a, Adjustment::BlackWhite { weights, tint: Some(_) } if weights[0] == 120.0 && weights[1] == 60.0));
        let a = from_params("hueSaturation", &json!({"blues": {"hue": 40}}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(&a, Adjustment::HueSaturation { ranges, .. } if ranges[4].hue == 40.0 && ranges[0].is_neutral()));
        let a = from_params("gradientMap", &json!({"stops": [[1, "#ffffff"], [0, "#000000"]], "dither": true}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(&a, Adjustment::GradientMap { stops, dither: true, .. } if stops[0].0 == 0.0));
        // CMYK curves: ink channels; Lab: lightness without a composite.
        let a = from_params("curves", &json!({"black": [[0, 0], [255, 200]]}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(&a, Adjustment::Curves { space: ToneSpace::Cmyk, black, .. } if black.len() == 2));
        let a = from_params("curves", &json!({"points": [[0, 40], [255, 255]]}), None, ColorMode::Lab).unwrap();
        assert!(matches!(&a, Adjustment::Curves { space: ToneSpace::Lab, per_channel, .. } if per_channel[0][0].output > 0.1));
        let a = from_params("levels", &json!({"inBlack": 20}), None, ColorMode::Cmyk).unwrap();
        assert!(matches!(&a, Adjustment::Levels { space: ToneSpace::Cmyk, master, .. } if (master.in_black - 20.0 / 255.0).abs() < 1e-6));
    }

    #[test]
    fn bad_params_are_errors_not_panics() {
        for (kind, p) in [
            ("curves", json!({"points": "abc"})),
            ("curves", json!({"points": [[0, 0]]})),
            ("curves", json!({"red": [[0, 0], [1]]})),
            ("curves", json!({"points": [[5, 5], [5, 9]]})),
            ("levels", json!({"red": 3})),
            ("levels", json!({"inBlack": "x"})),
            ("hueSaturation", json!({"reds": {"range": [1, 2]}})),
            ("hueSaturation", json!({"colorize": 1})),
            ("colorBalance", json!({"shadows": [1, 2]})),
            ("blackWhite", json!({"tintColor": "#zz0000"})),
            ("photoFilter", json!({"filter": "nope"})),
            ("photoFilter", json!({"color": [1]})),
            ("channelMixer", json!({"red": [1, 2, 3]})),
            ("gradientMap", json!({"stops": [[0, "#000"]]})),
            ("gradientMap", json!({"stops": 7})),
            ("nope", json!({})),
            ("levels", json!([1, 2])),
        ] {
            assert!(from_params(kind, &p, None, ColorMode::Rgb).is_err(), "{kind} {p}");
        }
        // Out-of-range numbers clamp.
        let a = from_params("hueSaturation", &json!({"hue": 1e9, "saturation": -1e9}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(a, Adjustment::HueSaturation { hue: 180.0, saturation: -100.0, .. }));
        let a = from_params("levels", &json!({"inBlack": 250, "inWhite": 10}), None, ColorMode::Rgb).unwrap();
        assert!(matches!(a, Adjustment::Levels { master, .. } if master.in_white > master.in_black));
    }

    #[test]
    fn hue_range_sliders_stay_ordered() {
        let a = from_params("hueSaturation", &json!({"greens": {"range": [100, 90, 200, 150]}}), None, ColorMode::Rgb).unwrap();
        let Adjustment::HueSaturation { ranges, .. } = a else { panic!() };
        let b = ranges[2].bounds;
        assert!(b[0] <= b[1] && b[1] <= b[2] && b[2] <= b[3] && b[3] - b[0] < 360.0, "{b:?}");
    }

    /// A colourful 16×16 document (`mode`, `depth`) for command tests.
    fn colourful(mode: &str, depth: u32) -> crate::Session {
        let mut s = crate::Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16, "mode": mode, "depth": depth})).unwrap();
        for (i, c) in ["#c83c28", "#3c8c50", "#2850b4", "#d2b464"].iter().enumerate() {
            s.execute("select.rect", json!({"x": (i % 2) * 8, "y": (i / 2) * 8, "width": 8, "height": 8})).unwrap();
            s.execute("edit.fill", json!({"color": c})).unwrap();
        }
        s.execute("select.deselect", json!({})).unwrap();
        s
    }

    fn pixels(s: &crate::Session) -> Vec<Vec<f32>> {
        let d = s.active().unwrap();
        let bg = d.doc.layers[0].surface().unwrap();
        [(2, 2), (10, 2), (2, 10), (10, 10)].iter().map(|&(x, y)| bg.pixel(x, y)).collect()
    }

    #[test]
    fn destructive_commands_apply_their_params_at_every_depth() {
        for depth in [8, 16, 32] {
            for kind in KINDS.iter().filter(|k| **k != "invert") {
                let mut s = colourful("rgb", depth);
                let before = pixels(&s);
                let steps = s.active().unwrap().history.past_len();
                s.execute(&format!("image.adjustments.{kind}"), sample(kind)).unwrap_or_else(|e| panic!("{kind} @{depth}: {e}"));
                assert_ne!(pixels(&s), before, "{kind} @{depth} changed nothing");
                assert_eq!(s.active().unwrap().history.past_len(), steps + 1, "{kind}: one history step");
                // Identity params leave the pixels alone (within one 8-bit step).
                let mut s = colourful("rgb", depth);
                s.execute(&format!("image.adjustments.{kind}"), to_params(&default_for(kind, ColorMode::Rgb).unwrap())).unwrap();
                if !matches!(*kind, "threshold" | "posterize" | "blackWhite" | "gradientMap" | "photoFilter") {
                    for (a, b) in pixels(&s).iter().zip(&before) {
                        assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1.0 / 255.0 + 1e-4), "{kind} @{depth}: {a:?} vs {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn cmyk_and_lab_documents_use_their_channels() {
        let mut s = colourful("cmyk", 8);
        let before = pixels(&s);
        // More black ink darkens every patch.
        s.execute("image.adjustments.curves", json!({"black": [[0, 0], [255, 180]]})).unwrap();
        let after = pixels(&s);
        assert!(after.iter().zip(&before).all(|(a, b)| a[3] > b[3] + 0.05), "{after:?} vs {before:?}");
        let mut s = colourful("lab", 16);
        let before = pixels(&s);
        s.execute("image.adjustments.levels", json!({"lightness": {"outBlack": 60}})).unwrap();
        let after = pixels(&s);
        assert!(after.iter().zip(&before).all(|(a, b)| a[0] >= b[0]), "lightness lifted {after:?} vs {before:?}");
        // The adjustment layer of a CMYK document defaults to ink channels.
        let mut s = colourful("cmyk", 8);
        s.execute("layer.newAdjustmentLayer.curves", json!({})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        let l = s.active().unwrap().doc.layer(id).unwrap().clone();
        assert!(matches!(l.content, photocraft_doc::LayerContent::Adjustment(Adjustment::Curves { space: ToneSpace::Cmyk, .. })));
    }

    #[test]
    fn set_adjustment_merges_and_resets() {
        let mut s = colourful("rgb", 8);
        s.execute("layer.newAdjustmentLayer.colorBalance", json!({"midtones": [30, 0, 0]})).unwrap();
        s.execute("layer.setAdjustment", json!({"highlights": [0, 0, -20]})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        let adj = |s: &crate::Session| match &s.active().unwrap().doc.layer(id).unwrap().content {
            photocraft_doc::LayerContent::Adjustment(a) => a.clone(),
            _ => panic!(),
        };
        assert!(matches!(adj(&s), Adjustment::ColorBalance { midtones: [30.0, 0.0, 0.0], highlights: [0.0, 0.0, -20.0], .. }));
        s.execute("layer.setAdjustment", json!({"layer": id.0})).unwrap();
        assert_eq!(adj(&s), default_for("colorBalance", ColorMode::Rgb).unwrap());
        // Bad params are errors and leave the layer alone.
        s.execute("layer.setAdjustment", json!({"midtones": [10, 10, 10]})).unwrap();
        let kept = adj(&s);
        for bad in [json!({"midtones": "x"}), json!({"preserveLuminosity": 3}), json!({"shadows": [1]})] {
            assert!(s.execute("layer.setAdjustment", bad.clone()).is_err(), "{bad}");
            assert_eq!(adj(&s), kept);
        }
    }

    #[test]
    fn commands_reject_bad_params_gracefully() {
        let mut s = colourful("rgb", 8);
        for (kind, p) in [
            ("curves", json!({"points": [[0, 0]]})),
            ("levels", json!({"red": "dark"})),
            ("gradientMap", json!({"stops": [[0, 1, 2]]})),
            ("photoFilter", json!({"filter": 3})),
            ("channelMixer", json!({"gray": [1, 2]})),
            ("hueSaturation", json!({"reds": 4})),
            ("blackWhite", json!({"tint": "yes"})),
        ] {
            for prefix in ["image.adjustments.", "layer.newAdjustmentLayer."] {
                let r = s.execute(&format!("{prefix}{kind}"), p.clone());
                assert!(matches!(r, Err(EngineError::BadParams { .. })), "{prefix}{kind} {p}: {r:?}");
            }
        }
    }
}
