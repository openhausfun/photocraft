//! Tagged-block helpers: the preserved block-list encoding, fills, text,
//! smart objects, locks and label colors.

use photocraft_color::{Color, ColorMode};
use photocraft_doc::{BlendIf, BlendRange, Fill, GradientStyle, LabelColor, Locks};
use photocraft_geom::Affine;
use photocraft_psd::descriptor::{Descriptor, Id, UnicodeString, Value, VersionedDescriptor};
use photocraft_psd::layer::BlendingRanges;

/// `brst` (channel blending restrictions): a list of big-endian u32 channel indices left out of
/// blending → a bit mask (bit `i` = channel `i`; indices above 31 are ignored).
pub fn parse_brst(data: &[u8]) -> u32 {
    data.as_chunks::<4>().0.iter().map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).filter(|&i| i < 32).fold(0, |m, i| m | 1 << i)
}

/// Inverse of [`parse_brst`]; `None` when every channel blends (no block).
pub fn brst_data(mask: u32) -> Option<Vec<u8>> {
    (mask != 0).then(|| (0..32u32).filter(|i| mask & 1 << i != 0).flat_map(u32::to_be_bytes).collect())
}

/// Layer-record blending ranges (Adobe PSD spec, "Layer blending ranges data": the composite
/// gray source and destination ranges, then a source and destination range per channel, each
/// as black low, black high, white low, white high) → Blend If. Full ranges map to the
/// default, so files without Blend If keep an empty setting.
pub fn blend_if_from_ranges(r: &BlendingRanges) -> BlendIf {
    let mut ranges: Vec<[BlendRange; 2]> = r.ranges().iter().map(|e| [BlendRange::from_bytes(e.source), BlendRange::from_bytes(e.dest)]).collect();
    // Trailing full entries carry nothing (export pads them back), so they are dropped, as
    // `BlendIf::set` does.
    let keep = ranges.iter().rposition(|p| !p.iter().all(BlendRange::is_full)).map_or(0, |i| i + 1);
    ranges.truncate(keep);
    BlendIf { ranges }
}

/// Inverse of [`blend_if_from_ranges`] for a document with `channels` colour channels: one
/// entry for gray plus one per channel (more when the setting stores more), unset entries full.
pub fn ranges_from_blend_if(b: &BlendIf, channels: usize) -> BlendingRanges {
    if b.is_default() {
        return BlendingRanges::full(channels);
    }
    let n = b.ranges.len().max(channels + 1);
    let data = (0..n).flat_map(|i| {
        let [src, dst] = b.get(i);
        src.to_bytes().into_iter().chain(dst.to_bytes())
    });
    BlendingRanges { data: data.collect() }
}

/// `lspf` bits (Adobe spec: bit 0 transparency, 1 composite, 2 position).
/// Artboard (bit 3... here `0x10` as observed by ag-psd) and "all" (bit 31)
/// are undocumented.
pub fn locks_from_lspf(v: u32) -> Locks {
    Locks { transparency: v & 1 != 0, pixels: v & 2 != 0, position: v & 4 != 0, artboard: v & 0x10 != 0, all: v & 0x8000_0000 != 0 }
}

/// Inverse of [`locks_from_lspf`].
pub fn lspf_from_locks(l: &Locks) -> u32 {
    u32::from(l.transparency) | u32::from(l.pixels) << 1 | u32::from(l.position) << 2 | u32::from(l.artboard) << 4 | u32::from(l.all) << 31
}

/// `lclr` index → label.
pub fn label_from_index(v: u16) -> LabelColor {
    match v {
        1 => LabelColor::Red,
        2 => LabelColor::Orange,
        3 => LabelColor::Yellow,
        4 => LabelColor::Green,
        5 => LabelColor::Blue,
        6 => LabelColor::Violet,
        7 => LabelColor::Gray,
        _ => LabelColor::None,
    }
}

/// Label → `lclr` index.
pub fn label_index(l: LabelColor) -> u16 {
    match l {
        LabelColor::None => 0,
        LabelColor::Red => 1,
        LabelColor::Orange => 2,
        LabelColor::Yellow => 3,
        LabelColor::Green => 4,
        LabelColor::Blue => 5,
        LabelColor::Violet => 6,
        LabelColor::Gray => 7,
    }
}

pub(crate) fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Double(d) => Some(*d),
        Value::UnitFloat { value, .. } => Some(*value),
        Value::Integer(i) => Some(f64::from(*i)),
        _ => None,
    }
}

/// Descriptor color (`RGBC`, `CMYC`, `Grsc`, `HSBC`) → [`Color`].
pub fn color_from_desc(d: &Descriptor) -> Option<Color> {
    let id = d.class_id.as_bytes();
    let g = |k: &str| num(d.get(k)).map(|v| v as f32);
    Some(match id {
        // Newer Photoshop writes 0..1 floats (`RGBC` with redFloat / greenFloat / blueFloat).
        b"RGBC" if d.get("redFloat").is_some() => Color::rgb(g("redFloat")?, g("greenFloat")?, g("blueFloat")?),
        b"RGBC" => Color::rgb(g("Rd  ")? / 255.0, g("Grn ")? / 255.0, g("Bl  ")? / 255.0),
        b"CMYC" => Color { mode: ColorMode::Cmyk, c: [g("Cyn ")? / 100.0, g("Mgnt")? / 100.0, g("Ylw ")? / 100.0, g("Blck")? / 100.0], alpha: 1.0 },
        b"Grsc" => Color::gray(1.0 - g("Gry ")? / 100.0),
        // CIE L*a*b*: L 0..100, a / b −128..127 (stored like Lab pixels: offset by 128).
        b"LbCl" => Color { mode: ColorMode::Lab, c: [g("Lmnc")? / 100.0, (g("A   ")? + 128.0) / 255.0, (g("B   ")? + 128.0) / 255.0, 0.0], alpha: 1.0 },
        // Newer Photoshop writes 0..1 floats.
        _ if d.get("redFloat").is_some() => Color::rgb(g("redFloat")?, g("greenFloat")?, g("blueFloat")?),
        _ => {
            // Fall back to any RGB-like triple.
            Color::rgb(g("Rd  ")? / 255.0, g("Grn ")? / 255.0, g("Bl  ")? / 255.0)
        }
    })
}

/// [`Color`] → descriptor in its own model.
pub fn color_to_desc(c: &Color) -> Descriptor {
    let d = |v: f32| Value::Double(f64::from(v));
    match c.mode {
        ColorMode::Cmyk => Descriptor::new("CMYC")
            .with("Cyn ", d(c.c[0] * 100.0))
            .with("Mgnt", d(c.c[1] * 100.0))
            .with("Ylw ", d(c.c[2] * 100.0))
            .with("Blck", d(c.c[3] * 100.0)),
        ColorMode::Grayscale => Descriptor::new("Grsc").with("Gry ", d((1.0 - c.c[0]) * 100.0)),
        ColorMode::Lab => {
            Descriptor::new("LbCl").with("Lmnc", d(c.c[0] * 100.0)).with("A   ", d(c.c[1] * 255.0 - 128.0)).with("B   ", d(c.c[2] * 255.0 - 128.0))
        }
        _ => {
            let rgb = c.to_rgb();
            Descriptor::new("RGBC").with("Rd  ", d(rgb[0] * 255.0)).with("Grn ", d(rgb[1] * 255.0)).with("Bl  ", d(rgb[2] * 255.0))
        }
    }
}

pub(crate) fn get_desc<'a>(d: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    match d.get(key)? {
        Value::Descriptor(x) | Value::GlobalObject(x) => Some(x),
        _ => None,
    }
}

/// Enumerated value of `key` (4-char or long id), if present.
pub(crate) fn enum_of<'a>(d: &'a Descriptor, key: &str) -> Option<&'a [u8]> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value.as_bytes()),
        _ => None,
    }
}

pub(crate) fn bool_of(d: &Descriptor, key: &str) -> bool {
    matches!(d.get(key), Some(Value::Boolean(true)))
}

/// `GrdT` gradient type.
pub(crate) fn gradient_style(d: &Descriptor) -> GradientStyle {
    match enum_of(d, "Type") {
        Some(b"Rdl ") => GradientStyle::Radial,
        Some(b"Angl") => GradientStyle::Angle,
        Some(b"Rflc") => GradientStyle::Reflected,
        Some(b"Dmnd") => GradientStyle::Diamond,
        _ => GradientStyle::Linear,
    }
}

pub(crate) fn gradient_style_value(s: GradientStyle) -> Value {
    let v = match s {
        GradientStyle::Linear => "Lnr ",
        GradientStyle::Radial => "Rdl ",
        GradientStyle::Angle => "Angl",
        GradientStyle::Reflected => "Rflc",
        GradientStyle::Diamond => "Dmnd",
    };
    Value::Enumerated { type_id: Id::new("GrdT"), value: Id::new(v) }
}

/// Colour and opacity stops of a `Grdn` descriptor (locations 0..4096 → 0..1).
/// Colour stops and opacity stops of a gradient.
pub(crate) type Stops = (Vec<(f32, Color)>, Vec<(f32, f32)>);

/// Stops of a `Grdn` descriptor, baked to Photoshop's interpolation (smoothness `Intr`, stop
/// midpoints, and the parent's `gs99` interpolation method when given).
pub(crate) fn gradient_stops_with(grad: &Descriptor, method: Option<&[u8]>) -> Stops {
    let raw = gradient_stops_raw(grad);
    let stops = if raw.stops.len() >= 2 {
        crate::gradient_bake::bake(raw.stops, &raw.mids, raw.smooth, crate::gradient_bake::Method::from_code(method))
    } else {
        raw.stops
    };
    (stops, raw.opacity)
}

/// A `Grdn` descriptor's stops as stored: colour stops (sorted), the midpoint of each segment
/// between them, opacity stops and the smoothness (`Intr`, `0..=1`).
pub(crate) struct RawStops {
    pub stops: Vec<(f32, Color)>,
    pub mids: Vec<f32>,
    pub opacity: Vec<(f32, f32)>,
    pub smooth: f32,
}

/// Stops of a gradient fill layer, kept editable: when Photoshop's interpolation is plain
/// (no smoothness, Classic method) the stops stay as they are with their midpoints, else they
/// are baked like [`gradient_stops_with`] (midpoints folded in). Opacity stops that are all
/// 100 % come back empty.
pub(crate) fn gradient_stops_editable(grad: &Descriptor, method: Option<&[u8]>) -> RawStops {
    let mut raw = gradient_stops_raw(grad);
    let m = crate::gradient_bake::Method::from_code(method);
    // Lab stops (Lab documents) interpolate in L*a*b*, which only baking reproduces.
    let lab = raw.stops.iter().all(|(_, c)| c.mode == photocraft_color::ColorMode::Lab);
    if raw.stops.len() >= 2 && (raw.smooth > 0.0 || m != crate::gradient_bake::Method::Classic || lab) {
        raw.stops = crate::gradient_bake::bake(std::mem::take(&mut raw.stops), &raw.mids, raw.smooth, m);
        raw.mids.clear();
    }
    if raw.mids.iter().all(|m| (m - 0.5).abs() < 1e-3) {
        raw.mids.clear();
    }
    if raw.opacity.iter().all(|o| (o.1 - 1.0).abs() < 1e-4) {
        raw.opacity.clear();
    }
    raw
}

fn gradient_stops_raw(grad: &Descriptor) -> RawStops {
    let mut stops = Vec::new();
    let mut mids = Vec::new();
    let mut opacity = Vec::new();
    if let Some(Value::List(items)) = grad.get("Clrs") {
        for it in items {
            if let Value::Descriptor(s) = it {
                let loc = num(s.get("Lctn")).unwrap_or(0.0) as f32 / 4096.0;
                if let Some(c) = get_desc(s, "Clr ").and_then(color_from_desc) {
                    stops.push((loc, c));
                    mids.push(num(s.get("Mdpn")).unwrap_or(50.0) as f32 / 100.0);
                }
            }
        }
    }
    // Midpoint k applies to the segment after stop k.
    let mut order: Vec<usize> = (0..stops.len()).collect();
    order.sort_by(|a, b| stops[*a].0.total_cmp(&stops[*b].0));
    let mids: Vec<f32> = order.iter().skip(1).map(|i| mids.get(*i).copied().unwrap_or(0.5)).collect();
    let smooth = num(grad.get("Intr")).map_or(0.0, |v| (v / 4096.0) as f32);
    let mut sorted: Vec<(f32, Color)> = order.iter().filter_map(|i| stops.get(*i).cloned()).collect();
    if sorted.len() != stops.len() {
        sorted = stops;
    }
    if let Some(Value::List(items)) = grad.get("Trns") {
        for it in items {
            if let Value::Descriptor(s) = it {
                let loc = num(s.get("Lctn")).unwrap_or(0.0) as f32 / 4096.0;
                opacity.push((loc, num(s.get("Opct")).unwrap_or(100.0) as f32 / 100.0));
            }
        }
    }
    RawStops { stops: sorted, mids, opacity, smooth }
}

/// A `Grdn` descriptor for colour and opacity stops.
pub(crate) fn gradient_desc(stops: &[(f32, Color)], opacity: &[(f32, f32)]) -> Descriptor {
    gradient_desc_with(stops, opacity, &[])
}

/// [`gradient_desc`] with colour midpoints (one per segment between the sorted stops; each is
/// stored on the stop that ends its segment, as [`gradient_stops_with`] reads it).
pub(crate) fn gradient_desc_with(stops: &[(f32, Color)], opacity: &[(f32, f32)], mids: &[f32]) -> Descriptor {
    let mut stops = stops.to_vec();
    if !mids.is_empty() {
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    let clrs = stops
        .iter()
        .enumerate()
        .map(|(i, (t, c))| {
            let mid = i.checked_sub(1).and_then(|k| mids.get(k)).copied().unwrap_or(0.5);
            Value::Descriptor(
                Descriptor::new("Clrt")
                    .with("Clr ", Value::Descriptor(color_to_desc(c)))
                    .with("Type", Value::Enumerated { type_id: Id::new("Clry"), value: Id::new("UsrS") })
                    .with("Lctn", Value::Integer((t * 4096.0).round() as i32))
                    .with("Mdpn", Value::Integer((mid.clamp(0.05, 0.95) * 100.0).round() as i32)),
            )
        })
        .collect();
    let op: Vec<(f32, f32)> = if opacity.is_empty() { stops.iter().map(|(t, c)| (*t, c.alpha)).collect() } else { opacity.to_vec() };
    let trns = op
        .iter()
        .map(|(t, a)| {
            Value::Descriptor(
                Descriptor::new("TrnS")
                    .with("Opct", Value::UnitFloat { unit: *b"#Prc", value: f64::from(a * 100.0) })
                    .with("Lctn", Value::Integer((t * 4096.0).round() as i32))
                    .with("Mdpn", Value::Integer(50)),
            )
        })
        .collect();
    Descriptor::new("Grdn")
        .with("Nm  ", Value::Text(UnicodeString::new_nul("Custom")))
        .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("CstS") })
        // Our stops interpolate linearly (Photoshop smoothness is baked into them on import).
        .with("Intr", Value::Double(0.0))
        .with("Clrs", Value::List(clrs))
        .with("Trns", Value::List(trns))
}

/// Opacity stops of a `Grdn` descriptor with Photoshop's smoothness and midpoints applied (the
/// same curve as the colour stops, Classic interpolation), densely sampled.
fn smoothed_opacity_stops(grad: &Descriptor) -> Vec<(f32, f32)> {
    let mut stops = Vec::new();
    let mut mids = Vec::new();
    if let Some(Value::List(items)) = grad.get("Trns") {
        for it in items {
            if let Value::Descriptor(s) = it {
                let a = num(s.get("Opct")).unwrap_or(100.0) as f32 / 100.0;
                stops.push((num(s.get("Lctn")).unwrap_or(0.0) as f32 / 4096.0, Color::rgba(a, a, a, 1.0)));
                mids.push(num(s.get("Mdpn")).unwrap_or(50.0) as f32 / 100.0);
            }
        }
    }
    let mut order: Vec<usize> = (0..stops.len()).collect();
    order.sort_by(|a, b| stops[*a].0.total_cmp(&stops[*b].0));
    let mids: Vec<f32> = order.iter().skip(1).filter_map(|i| mids.get(*i).copied()).collect();
    let smooth = num(grad.get("Intr")).map_or(0.0, |v| (v / 4096.0) as f32);
    let baked = if stops.len() >= 2 { crate::gradient_bake::bake(stops, &mids, smooth, crate::gradient_bake::Method::Classic) } else { stops };
    baked.into_iter().map(|(t, c)| (t, c.to_rgb()[0])).collect()
}

/// Parses a fill block (`SoCo`, `GdFl`, `PtFl`).
pub fn parse_fill(key: &[u8; 4], data: &[u8]) -> Option<Fill> {
    // Block data may carry trailing padding after the descriptor.
    let d = parse_prefix_versioned(data)?;
    fill_from_desc(key, &d)
}

/// A fill from its descriptor (`SoCo`/`GdFl`/`PtFl` layout; also the `solidColorLayer`,
/// `gradientLayer` and `patternLayer` contents of shape strokes).
pub fn fill_from_desc(key: &[u8; 4], d: &Descriptor) -> Option<Fill> {
    match key {
        b"SoCo" => Some(Fill::Solid(color_from_desc(get_desc(d, "Clr ")?)?)),
        b"GdFl" => {
            let angle = num(d.get("Angl")).unwrap_or(90.0) as f32;
            let scale = num(d.get("Scl ")).map_or(1.0, |v| v as f32 / 100.0);
            let raw = get_desc(d, "Grad").map(|g| gradient_stops_editable(g, enum_of(d, "gs99")));
            let (mut stops, midpoints) = raw.map(|r| (r.stops, r.mids)).unwrap_or_default();
            // Opacity stops with Photoshop's smoothness and opacity midpoints applied (raw when
            // the interpolation is plain); none when fully opaque.
            let mut opacity_stops = get_desc(d, "Grad").map(smoothed_opacity_stops).unwrap_or_default();
            if opacity_stops.iter().all(|o| o.1 >= 1.0 - 1e-4) {
                opacity_stops.clear();
            }
            if stops.is_empty() {
                stops = vec![(0.0, Color::BLACK), (1.0, Color::WHITE)];
            }
            let pct = |o: &Descriptor, k: &str| num(o.get(k)).map_or(0.0, |v| v as f32 / 100.0);
            let offset = get_desc(d, "Ofst").map_or((0.0, 0.0), |o| (pct(o, "Hrzn"), pct(o, "Vrtc")));
            Some(Fill::Gradient {
                stops,
                angle,
                scale,
                style: gradient_style(d),
                reverse: bool_of(d, "Rvrs"),
                opacity_stops,
                midpoints,
                offset,
                dither: bool_of(d, "Dthr"),
                align: !matches!(d.get("Algn"), Some(Value::Boolean(false))),
            })
        }
        b"PtFl" => {
            let (name, id) = pattern_ref(d);
            let scale = num(d.get("Scl ")).map_or(1.0, |v| v as f32 / 100.0);
            let (angle, link, phase) = pattern_placement(d);
            Some(Fill::Pattern { name, scale, id, angle, link, phase })
        }
        _ => None,
    }
}

/// Serializes a fill to its block.
pub fn write_fill(f: &Fill) -> ([u8; 4], Vec<u8>) {
    let (k, d) = fill_to_desc(f);
    (k, VersionedDescriptor::new(d).to_bytes())
}

/// A fill as (block key, descriptor with class `null`).
pub fn fill_to_desc(f: &Fill) -> ([u8; 4], Descriptor) {
    match f {
        Fill::Solid(c) => (*b"SoCo", Descriptor::new("null").with("Clr ", Value::Descriptor(color_to_desc(c)))),
        Fill::Gradient { stops, angle, scale, style, reverse, opacity_stops, midpoints, offset, dither, align } => {
            let prc = |v: f32| Value::UnitFloat { unit: *b"#Prc", value: f64::from(v * 100.0) };
            let mut d = Descriptor::new("null")
                .with("Angl", Value::UnitFloat { unit: *b"#Ang", value: f64::from(*angle) })
                .with("Type", gradient_style_value(*style))
                .with("Scl ", prc(*scale))
                .with("Grad", Value::Descriptor(gradient_desc_with(stops, opacity_stops, midpoints)));
            if *reverse {
                d = d.with("Rvrs", Value::Boolean(true));
            }
            if *dither {
                d = d.with("Dthr", Value::Boolean(true));
            }
            if !*align {
                d = d.with("Algn", Value::Boolean(false));
            }
            if *offset != (0.0, 0.0) {
                d = d.with("Ofst", Value::Descriptor(Descriptor::new("Pnt ").with("Hrzn", prc(offset.0)).with("Vrtc", prc(offset.1))));
            }
            (*b"GdFl", d)
        }
        Fill::Pattern { name, scale, id, angle, link, phase } => {
            let d = Descriptor::new("null")
                .with("Scl ", Value::UnitFloat { unit: *b"#Prc", value: f64::from(scale * 100.0) })
                .with("Ptrn", Value::Descriptor(pattern_ref_desc(name, id)));
            (*b"PtFl", with_pattern_placement(d, *angle, *link, *phase))
        }
    }
}

/// Pattern name and id (`Ptrn` → `Nm  `, `Idnt`) of a pattern fill / overlay descriptor.
pub(crate) fn pattern_ref(d: &Descriptor) -> (String, String) {
    let p = get_desc(d, "Ptrn");
    let text = |k: &str| match p.and_then(|p| p.get(k)) {
        Some(Value::Text(t)) => t.to_string_lossy(),
        _ => String::new(),
    };
    (text("Nm  "), text("Idnt"))
}

/// The `Ptrn` reference descriptor (the id is omitted when unknown).
pub(crate) fn pattern_ref_desc(name: &str, id: &str) -> Descriptor {
    let p = Descriptor::new("Ptrn").with("Nm  ", Value::Text(UnicodeString::new_nul(name)));
    if id.is_empty() { p } else { p.with("Idnt", Value::Text(UnicodeString::new_nul(id))) }
}

/// Angle (`Angl`, degrees), link with layer (`Algn`, default on) and phase (`phase` point) of a
/// pattern fill / overlay descriptor.
pub(crate) fn pattern_placement(d: &Descriptor) -> (f32, bool, (f32, f32)) {
    let angle = num(d.get("Angl")).unwrap_or(0.0) as f32;
    let link = !matches!(d.get("Algn"), Some(Value::Boolean(false)));
    let phase = get_desc(d, "phase").map_or((0.0, 0.0), |p| (num(p.get("Hrzn")).unwrap_or(0.0) as f32, num(p.get("Vrtc")).unwrap_or(0.0) as f32));
    (angle, link, phase)
}

/// Adds the keys read by [`pattern_placement`] (angle only when non-zero).
pub(crate) fn with_pattern_placement(mut d: Descriptor, angle: f32, link: bool, phase: (f32, f32)) -> Descriptor {
    if angle != 0.0 {
        d = d.with("Angl", Value::UnitFloat { unit: *b"#Ang", value: f64::from(angle) });
    }
    d.with("Algn", Value::Boolean(link)).with(
        "phase",
        Value::Descriptor(Descriptor::new("Pnt ").with("Hrzn", Value::Double(f64::from(phase.0))).with("Vrtc", Value::Double(f64::from(phase.1)))),
    )
}

/// The warp of a placed layer (`SoLd`/`SoLE` `warp` descriptor): style, bend, distortions,
/// axis, bounds and, for `warpCustom`, the 4 × 4 `customEnvelopeWarp` mesh. Read-only: the raw
/// block is what gets written back. `None` when absent or `warpNone` with no mesh.
pub fn parse_placed_warp(key: &[u8; 4], data: &[u8]) -> Option<photocraft_geom::warp::Warp> {
    use photocraft_geom::warp::{BezierMesh, Warp, WarpStyle};
    if key != b"SoLd" && key != b"SoLE" {
        return None;
    }
    let d = data.get(8..).and_then(parse_prefix_versioned)?;
    let w = get_desc(&d, "warp")?;
    let style = enum_of(w, "warpStyle").and_then(|v| WarpStyle::parse(&String::from_utf8_lossy(v)))?;
    let b = get_desc(w, "bounds")?;
    let g = |k: &str| num(b.get(k));
    let bounds = [g("Left")?, g("Top ")?, g("Rght")?, g("Btom")?];
    let mut warp = Warp::preset(style, num(w.get("warpValue")).unwrap_or(0.0), bounds);
    warp.h_distort = num(w.get("warpPerspective")).unwrap_or(0.0);
    warp.v_distort = num(w.get("warpPerspectiveOther")).unwrap_or(0.0);
    warp.vertical = enum_of(w, "warpRotate") == Some(b"Vrtc");
    if style == WarpStyle::Custom {
        let env = get_desc(w, "customEnvelopeWarp")?;
        let Some(Value::ObjectArray(arr)) = env.get("meshPoints") else { return None };
        let vals = |k: &str| match arr.body.get(k) {
            Some(Value::UnitFloats { values, .. }) => Some(values.clone()),
            _ => None,
        };
        let (xs, ys) = (vals("Hrzn")?, vals("Vrtc")?);
        if xs.len() != 16 || ys.len() != 16 {
            return None;
        }
        let mesh = BezierMesh { us: vec![0.0, 1.0], vs: vec![0.0, 1.0], points: xs.iter().zip(&ys).map(|(x, y)| [*x, *y]).collect() };
        warp.mesh = mesh.is_valid().then_some(mesh);
        warp.mesh.as_ref()?;
    }
    (!warp.is_identity()).then_some(warp)
}

/// Text content and transform from a `TySh` block (best effort).
pub fn parse_tysh(data: &[u8]) -> Option<(String, Affine)> {
    let f = |at: usize| data.get(at..at + 8).map(|b| f64::from_be_bytes(b.try_into().unwrap_or([0; 8])));
    let mut m = [0.0; 6];
    for (i, v) in m.iter_mut().enumerate() {
        *v = f(2 + i * 8)?;
    }
    // text version (2) at 50, descriptor version (4) at 52, descriptor at 56.
    let rest = data.get(52..)?;
    let text = parse_prefix_versioned(rest).and_then(|d| match d.get("Txt ") {
        Some(Value::Text(t)) => Some(t.to_string_lossy()),
        _ => None,
    });
    Some((text.unwrap_or_default(), Affine { m }))
}

/// Parses a version-16 descriptor at the start of `data` (trailing bytes allowed).
pub(crate) fn parse_prefix_versioned(data: &[u8]) -> Option<Descriptor> {
    VersionedDescriptor::parse_prefix(data).ok().map(|(v, _)| v.descriptor)
}

/// Smart object identifier and transform from `SoLd`/`PlLd` (best effort).
pub fn parse_smart(key: &[u8; 4], data: &[u8]) -> (String, Affine) {
    // SoLd: 'soLD' + version(4) + versioned descriptor.
    let desc = if key == b"SoLd" || key == b"SoLE" { data.get(8..).and_then(parse_prefix_versioned) } else { None };
    let Some(d) = desc else { return (String::new(), Affine::IDENTITY) };
    let id = match d.get("Idnt") {
        Some(Value::Text(t)) => t.to_string_lossy(),
        _ => String::new(),
    };
    let mut affine = Affine::IDENTITY;
    if let (Some(Value::List(pts)), Some(sz)) = (d.get("Trnf"), get_desc(&d, "Sz  ")) {
        let p: Vec<f64> = pts.iter().filter_map(|v| num(Some(v))).collect();
        let (w, h) = (num(sz.get("Wdth")).unwrap_or(0.0), num(sz.get("Hght")).unwrap_or(0.0));
        if p.len() == 8 && w > 0.0 && h > 0.0 {
            affine = Affine { m: [(p[2] - p[0]) / w, (p[3] - p[1]) / w, (p[6] - p[0]) / h, (p[7] - p[1]) / h, p[0], p[1]] };
        }
    }
    (id, affine)
}

/// `masterFXSwitch` from an `lfx2` block (defaults to `true`).
pub fn effects_enabled(lfx2: &[u8]) -> bool {
    lfx2.get(4..)
        .and_then(parse_prefix_versioned)
        .and_then(|d| match d.get("masterFXSwitch") {
            Some(Value::Boolean(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    /// psd-tools layers-minimal/gradient-fill.psd ("Color to Transparent", no stored pixels): a
    /// gradient fill's opacity stops reach the model (as its opacity stops; written from stop
    /// alpha) and survive a write; the ramp's alpha follows them.
    #[test]
    fn gradient_fill_opacity_stops_become_alpha() {
        use photocraft_color::Color;
        use photocraft_doc::{Fill, GradientStyle};
        let red = Color::rgba(1.0, 0.0, 0.0, 1.0);
        let f = Fill::gradient(vec![(0.0, red), (1.0, Color { alpha: 0.0, ..red })], 90.0, 1.0, GradientStyle::Linear, false);
        let (k, data) = super::write_fill(&f);
        let back = super::parse_fill(&k, &data).unwrap();
        let Fill::Gradient { stops, opacity_stops, .. } = &back else { panic!("gradient") };
        assert_eq!(opacity_stops, &vec![(0.0, 1.0), (1.0, 0.0)]);
        assert!(stops.iter().all(|s| s.1.c[0] == 1.0 && s.1.c[1] == 0.0));
        let ramp = photocraft_compose::gradient_fill::Ramp::new(&back).unwrap();
        assert_eq!(ramp.sample(0.0)[3], 1.0);
        assert!(ramp.sample(1.0)[3] < 0.01);
        // Opaque gradients come back without opacity stops.
        let (k, data) = super::write_fill(&Fill::gradient(vec![(0.0, red), (1.0, Color::WHITE)], 0.0, 1.0, GradientStyle::Linear, false));
        let Some(Fill::Gradient { opacity_stops, .. }) = super::parse_fill(&k, &data) else { panic!("gradient") };
        assert!(opacity_stops.is_empty());
    }

    #[test]
    fn lab_descriptor_colours_round_trip() {
        use photocraft_color::ColorMode;
        use photocraft_psd::descriptor::{Descriptor, Value};
        let d = Descriptor::new("LbCl").with("Lmnc", Value::Double(19.07)).with("A   ", Value::Double(52.29)).with("B   ", Value::Double(-85.08));
        let c = super::color_from_desc(&d).unwrap();
        assert_eq!(c.mode, ColorMode::Lab);
        assert!((c.c[0] - 0.1907).abs() < 1e-5 && (c.c[1] * 255.0 - 128.0 - 52.29).abs() < 1e-3 && (c.c[2] * 255.0 - 128.0 + 85.08).abs() < 1e-3);
        let back = super::color_from_desc(&super::color_to_desc(&c)).unwrap();
        assert_eq!(back.mode, ColorMode::Lab);
        for i in 0..3 {
            assert!((back.c[i] - c.c[i]).abs() < 1e-6);
        }
        // Missing components are rejected, not defaulted.
        assert!(super::color_from_desc(&Descriptor::new("LbCl").with("Lmnc", Value::Double(50.0))).is_none());
    }

    #[test]
    fn blending_ranges_map_to_blend_if() {
        use photocraft_doc::{BlendIf, BlendRange};
        use photocraft_psd::layer::BlendingRanges;
        // Full ranges (what every layer without Blend If carries) → the default setting.
        assert_eq!(super::blend_if_from_ranges(&BlendingRanges::full(3)), BlendIf::default());
        assert_eq!(super::blend_if_from_ranges(&BlendingRanges::default()), BlendIf::default());
        // Gray: This Layer black split 10/40; Blue (entry 3): Underlying white at 200.
        let mut data = BlendingRanges::full(3).data;
        data[..4].copy_from_slice(&[10, 40, 255, 255]);
        data[3 * 8 + 4..3 * 8 + 8].copy_from_slice(&[0, 0, 200, 200]);
        let b = super::blend_if_from_ranges(&BlendingRanges { data: data.clone() });
        assert_eq!(b.get(0), [BlendRange { black: [10, 40], white: [255, 255] }, BlendRange::FULL]);
        assert_eq!(b.get(3), [BlendRange::FULL, BlendRange { black: [0, 0], white: [200, 200] }]);
        assert_eq!(b.ranges.len(), 4);
        assert_eq!(super::ranges_from_blend_if(&b, 3).data, data);
        // The default writes full ranges for gray + each channel.
        assert_eq!(super::ranges_from_blend_if(&BlendIf::default(), 4), BlendingRanges::full(4));
        // A trimmed setting is padded to the document's channel count.
        let mut short = BlendIf::default();
        short.set(0, [BlendRange { black: [5, 5], white: [255, 255] }, BlendRange::FULL]);
        let r = super::ranges_from_blend_if(&short, 3);
        assert_eq!(r.data.len(), 4 * 8);
        assert_eq!(&r.data[..8], &[5, 5, 255, 255, 0, 0, 255, 255]);
        assert_eq!(&r.data[8..], &BlendingRanges::full(2).data[..]);
    }

    #[test]
    fn brst_round_trip() {
        assert_eq!(super::parse_brst(&[0, 0, 0, 2]), 0b100);
        assert_eq!(super::parse_brst(&[0, 0, 0, 0, 0, 0, 0, 1]), 0b11);
        assert_eq!(super::brst_data(0b101).unwrap(), vec![0, 0, 0, 0, 0, 0, 0, 2]);
        assert_eq!(super::brst_data(0), None);
    }

    use super::*;

    #[test]
    fn placed_layer_warp_is_read() {
        use photocraft_geom::warp::WarpStyle;
        use photocraft_psd::descriptor::ObjectArray;
        let e = |t: &str, v: &str| Value::Enumerated { type_id: Id::new(t), value: Id::new(v) };
        let bounds = Descriptor::new("Rctn")
            .with("Top ", Value::UnitFloat { unit: *b"#Pxl", value: 0.0 })
            .with("Left", Value::UnitFloat { unit: *b"#Pxl", value: 0.0 })
            .with("Btom", Value::UnitFloat { unit: *b"#Pxl", value: 30.0 })
            .with("Rght", Value::UnitFloat { unit: *b"#Pxl", value: 60.0 });
        let sold = |warp: Descriptor| {
            let mut v = b"soLD".to_vec();
            v.extend_from_slice(&4u32.to_be_bytes());
            v.extend(VersionedDescriptor::new(Descriptor::new("null").with("warp", Value::Descriptor(warp))).to_bytes());
            v
        };
        let arc = Descriptor::new("warp")
            .with("warpStyle", e("warpStyle", "warpArc"))
            .with("warpValue", Value::Double(40.0))
            .with("warpPerspective", Value::Double(0.0))
            .with("warpPerspectiveOther", Value::Double(0.0))
            .with("warpRotate", e("Ornt", "Hrzn"))
            .with("bounds", Value::Descriptor(bounds.clone()));
        let w = parse_placed_warp(b"SoLd", &sold(arc)).unwrap();
        assert_eq!((w.style, w.bend, w.bounds), (WarpStyle::Arc, 40.0, [0.0, 0.0, 60.0, 30.0]));
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for j in 0..4 {
            for i in 0..4 {
                xs.push(f64::from(i) * 20.0 + if i == 3 && j == 3 { 7.0 } else { 0.0 });
                ys.push(f64::from(j) * 10.0);
            }
        }
        let mesh = Descriptor::new("null")
            .with("Hrzn", Value::UnitFloats { unit: *b"#Pxl", values: xs })
            .with("Vrtc", Value::UnitFloats { unit: *b"#Pxl", values: ys });
        let custom = Descriptor::new("warp").with("warpStyle", e("warpStyle", "warpCustom")).with("bounds", Value::Descriptor(bounds)).with(
            "customEnvelopeWarp",
            Value::Descriptor(Descriptor::new("customEnvelopeWarp").with("meshPoints", Value::ObjectArray(ObjectArray { prefix: 16, body: mesh }))),
        );
        let w = parse_placed_warp(b"SoLd", &sold(custom)).unwrap();
        assert_eq!(w.style, WarpStyle::Custom);
        assert_eq!(w.mesh.as_ref().unwrap().points[15], [67.0, 30.0]);
        assert!(parse_placed_warp(b"PlLd", &[0; 20]).is_none());
    }

    #[test]
    fn locks_and_labels() {
        for bits in [0u32, 1, 2, 4, 0x10, 0x8000_0000, 0x8000_0017] {
            assert_eq!(lspf_from_locks(&locks_from_lspf(bits)), bits);
        }
        for i in 0..8 {
            assert_eq!(label_index(label_from_index(i)), i);
        }
    }

    #[test]
    fn fills_roundtrip() {
        for f in [
            Fill::Solid(Color::rgb(1.0, 0.5, 0.0)),
            Fill::Solid(Color { mode: ColorMode::Cmyk, c: [0.1, 0.2, 0.3, 0.4], alpha: 1.0 }),
            Fill::Solid(Color::gray(0.25)),
            Fill::gradient(vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))], 45.0, 1.0, GradientStyle::Reflected, true),
            // A live gradient (Gradient tool): canvas-aligned, offset, midpoints, opacity, dither.
            Fill::Gradient {
                stops: vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.5, Color::rgb(0.0, 1.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))],
                angle: -33.5,
                scale: 2.25,
                style: GradientStyle::Radial,
                reverse: false,
                opacity_stops: vec![(0.0, 1.0), (0.25, 0.5), (1.0, 0.0)],
                midpoints: vec![0.25, 0.75],
                offset: (0.125, -0.375),
                dither: true,
                align: false,
            },
            Fill::Pattern { name: "Bubbles".into(), scale: 0.5, id: "abc".into(), angle: 30.0, link: false, phase: (3.0, -2.0) },
        ] {
            let (k, d) = write_fill(&f);
            let back = parse_fill(&k, &d).unwrap();
            match (&f, &back) {
                (Fill::Solid(a), Fill::Solid(b)) => {
                    assert_eq!(a.mode, b.mode);
                    for i in 0..4 {
                        assert!((a.c[i] - b.c[i]).abs() < 1e-5);
                    }
                }
                _ => assert_eq!(back, f),
            }
        }
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn float_rgb_colors_parse() {
        let d = Descriptor::new("RGBC").with("redFloat", Value::Double(1.0)).with("greenFloat", Value::Double(0.5)).with("blueFloat", Value::Double(0.25));
        let c = color_from_desc(&d).unwrap();
        assert_eq!(c.to_rgb(), [1.0, 0.5, 0.25]);
    }

    #[test]
    fn smart_transform_from_sold() {
        let pts = [10.0, 20.0, 110.0, 20.0, 110.0, 70.0, 10.0, 70.0].map(Value::Double).to_vec();
        let d = Descriptor::new("null")
            .with("Idnt", Value::Text(UnicodeString::new_nul("uuid-1")))
            .with("Trnf", Value::List(pts))
            .with("Sz  ", Value::Descriptor(Descriptor::new("Pnt ").with("Wdth", Value::Double(50.0)).with("Hght", Value::Double(25.0))));
        let mut data = b"soLD".to_vec();
        data.extend_from_slice(&4u32.to_be_bytes());
        data.extend(VersionedDescriptor::new(d).to_bytes());
        data.extend_from_slice(&[0, 0]);
        let (id, a) = parse_smart(b"SoLd", &data);
        assert_eq!(id, "uuid-1");
        assert_eq!(a.m, [2.0, 0.0, 0.0, 2.0, 10.0, 20.0]);
        assert_eq!(parse_smart(b"PlLd", &data).0, "");
    }

    #[test]
    fn effects_switch() {
        let mut lfx = vec![0, 0, 0, 0];
        lfx.extend(VersionedDescriptor::new(Descriptor::new("null").with("masterFXSwitch", Value::Boolean(false))).to_bytes());
        assert!(!effects_enabled(&lfx));
        assert!(effects_enabled(&[0, 0, 0, 0]));
    }

    #[test]
    fn tysh_garbage_is_none() {
        assert!(parse_tysh(&[0; 10]).is_none());
    }
}
