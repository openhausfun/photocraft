//! Edit › Fill… (`edit.fill`): Photoshop's Fill dialog as one command.
//!
//! Contents: Foreground / Background / Color / Content-Aware / Pattern / History / Black /
//! 50% Gray / White, blended with `mode` at `opacity`, optionally preserving the target's
//! transparency. The selection (or the whole canvas) is filled, as one history step ("Fill").
//! Every parameter is validated: a bad one is an error, never a panic.

use photocraft_color::BlendMode;
use photocraft_geom::{Rect, TILE_SIZE};
use photocraft_raster::{Surface, from_rgba_into, to_rgba};
use serde_json::{Value, json};

use crate::commands::{blend_from_str, color_param, layer_param};
use crate::{EngineError, Result, Session};

const CMD: &str = "edit.fill";

/// Every `contents` value, in the dialog's order.
pub const CONTENTS: [&str; 9] = ["foreground", "background", "color", "contentAware", "pattern", "history", "black", "gray", "white"];

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// What a pixel of the filled area gets before blending.
enum Source {
    Color([f32; 4]),
    /// Straight RGBA per pixel of `rect` (row-major).
    Pixels(Rect, Vec<[f32; 4]>),
}

impl Source {
    fn at(&self, x: i32, y: i32) -> [f32; 4] {
        match self {
            Source::Color(c) => *c,
            Source::Pixels(r, px) => {
                if !r.contains(x, y) {
                    return [0.0; 4];
                }
                let i = (y - r.y0) as usize * r.width() as usize + (x - r.x0) as usize;
                px.get(i).copied().unwrap_or([0.0; 4])
            }
        }
    }
}

/// The `contents` param; without one, `color` (the old `{"color": …}` form, foreground by default).
fn contents(p: &Value) -> Result<&str> {
    match p.get("contents") {
        None | Some(Value::Null) => Ok("color"),
        Some(Value::String(c)) => {
            CONTENTS.iter().find(|k| k.eq_ignore_ascii_case(c)).copied().ok_or_else(|| bad(format!("unknown contents `{c}` ({})", CONTENTS.join("|"))))
        }
        Some(v) => Err(bad(format!("`contents` must be a string, not {v}"))),
    }
}

fn mode(p: &Value) -> Result<BlendMode> {
    match p.get("mode") {
        None | Some(Value::Null) => Ok(BlendMode::Normal),
        Some(Value::String(m)) => blend_from_str(m).filter(|m| *m != BlendMode::PassThrough).ok_or_else(|| bad(format!("unknown blend mode `{m}`"))),
        Some(v) => Err(bad(format!("`mode` must be a blend mode name, not {v}"))),
    }
}

/// Opacity in percent (0–100) → 0..1.
fn opacity(p: &Value) -> Result<f32> {
    match p.get("opacity") {
        None | Some(Value::Null) => Ok(1.0),
        Some(v) => {
            let o = v.as_f64().filter(|o| o.is_finite()).ok_or_else(|| bad(format!("`opacity` must be a number 0..100, not {v}")))?;
            Ok((o.clamp(0.0, 100.0) / 100.0) as f32)
        }
    }
}

fn flag(p: &Value, k: &str, d: bool) -> Result<bool> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(d),
        Some(Value::Bool(b)) => Ok(*b),
        Some(v) => Err(bad(format!("`{k}` must be true or false, not {v}"))),
    }
}

/// Edit › Fill.
pub fn fill(s: &mut Session, p: &Value) -> Result<Value> {
    let contents = contents(p)?;
    let mode = mode(p)?;
    let opacity = opacity(p)?;
    let preserve = flag(p, "preserveTransparency", false)?;
    let channel = crate::channel_cmds::is_channel_target(p);
    let id = if channel { None } else { Some(layer_param(s, p)?) };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc_bounds = st.doc.bounds();
    let sel = st.doc.selection.clone();
    let area = sel.as_ref().map(|m| m.content_bounds()).unwrap_or(doc_bounds);
    let mut out = json!({ "contents": contents });
    let restore = contents == "history";
    let source = match contents {
        "foreground" => Source::Color(s.tools.foreground),
        "background" => Source::Color(s.tools.background),
        "color" => Source::Color(color_param(p, "color", s.tools.foreground)),
        "black" => Source::Color([0.0, 0.0, 0.0, 1.0]),
        "gray" => Source::Color([128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0]),
        "white" => Source::Color([1.0; 4]),
        "pattern" => {
            let pat = crate::pattern_cmds::resolve_param(s, CMD, p)?;
            let (scale, angle, _, phase) = crate::pattern_cmds::placement(p);
            let tile = photocraft_compose::pattern::Tile::new(&pat).ok_or_else(|| bad("the pattern is empty"))?;
            // Photoshop's Fill uses the canvas origin.
            let place = photocraft_compose::pattern::Placement::new(Rect::EMPTY, false, phase, scale, angle);
            out["pattern"] = json!(pat.id);
            Source::Pixels(area, photocraft_compose::pattern::render(&tile, &place, area))
        }
        "history" => {
            let explicit = match p.get("state") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().ok_or_else(|| bad(format!("`state` must be a history state index, not {v}")))? as usize),
            };
            let past = st.history.past_len();
            let source_doc = match explicit {
                Some(i) if i == past => st.doc.clone(),
                Some(i) => st.history.state(i).ok_or_else(|| bad(format!("no history state {i} (0..={past})")))?,
                // The History Brush's default source: the oldest state still held.
                None => st.history.state(0).unwrap_or_else(|| st.doc.clone()),
            };
            let mut source_doc = (*source_doc).clone();
            let (src, _) = crate::channel_cmds::target_surface(&mut source_doc, id, p)
                .map_err(|_| EngineError::Other("the target did not exist (as pixels, a mask or a channel) in that history state".into()))?;
            Source::Pixels(area, read_rgba(src, area))
        }
        "contentAware" => {
            let sel = sel.as_ref().ok_or_else(|| EngineError::Other("Content-Aware fill needs a selection".into()))?;
            if channel || crate::commands::is_mask_target(p) {
                return Err(EngineError::Other("Content-Aware fill works on layer pixels only".into()));
            }
            let adapt = flag(p, "colorAdaptation", true)?;
            let lid = id.ok_or_else(|| EngineError::Other("no active layer".into()))?;
            let surf = st.doc.layer(lid).and_then(|l| l.surface()).ok_or_else(|| EngineError::Other("Content-Aware fill needs a pixel layer".into()))?;
            let seed = p.get("seed").and_then(Value::as_u64).unwrap_or(1);
            content_aware(surf, sel, doc_bounds, adapt, seed)?
        }
        _ => return Err(bad(format!("unknown contents `{contents}`"))),
    };
    let area = area.intersect(&doc_bounds);
    if area.is_empty() {
        return Ok(out);
    }
    s.edit("Fill", |doc, _| {
        let sel = doc.selection.clone();
        let (surf, lock) = crate::channel_cmds::target_surface(doc, id, p)?;
        blend_into(surf, area, &source, sel.as_ref(), Blend { mode, opacity, keep_alpha: preserve || lock, restore });
        Ok(())
    })?;
    Ok(out)
}

fn read_rgba(src: &Surface, area: Rect) -> Vec<[f32; 4]> {
    let mut px = vec![[0.0f32; 4]; area.width() as usize * area.height() as usize];
    src.read_rgba_into(area, &mut px);
    px
}

/// The selection filled from its surroundings (`photocraft_algo::content_aware`), as straight
/// RGBA over the sampling window. Only the selected pixels are used.
fn content_aware(surf: &Surface, sel: &Surface, canvas: Rect, color_adaptation: bool, seed: u64) -> Result<Source> {
    use photocraft_algo::content_aware::{FillOptions, color_level, fill, rotation_level};
    let hole_bounds = sel.content_bounds().intersect(&canvas);
    if hole_bounds.is_empty() {
        return Err(EngineError::Other("the selection is outside the canvas".into()));
    }
    let ext = hole_bounds.width().max(hole_bounds.height());
    let window = hole_bounds.inflate(i32::try_from(ext * 3 / 4).unwrap_or(i32::MAX).max(32)).intersect(&canvas);
    let fmt = surf.format();
    let n = fmt.channels();
    let (w, h) = (window.width() as usize, window.height() as usize);
    // Same cap as other whole-image operations: refuse absurd windows instead of allocating.
    if w.saturating_mul(h) > 400_000_000 {
        return Err(EngineError::Other("the selection is too large for Content-Aware fill".into()));
    }
    let img = surf.read_region(window);
    let mut hole = vec![false; w * h];
    for (i, hv) in hole.iter_mut().enumerate() {
        *hv = sel.sample_channel(window.x0 + (i % w) as i32, window.y0 + (i / w) as i32, 0) > 0.0;
    }
    if hole.iter().all(|h| *h) {
        return Err(EngineError::Other("Content-Aware fill needs pixels outside the selection to sample from".into()));
    }
    let source = vec![true; w * h];
    let opts = FillOptions {
        color_adaptation: color_level(if color_adaptation { "default" } else { "none" }),
        rotations: rotation_level("none"),
        scale: false,
        mirror: false,
        seed,
    };
    if img.len() != w * h * n {
        return Err(EngineError::Other("could not read the layer pixels".into()));
    }
    let filled = fill(w, h, n, &img, &hole, &source, &opts);
    let px = filled.chunks_exact(n.max(1)).map(|c| to_rgba(&fmt, c)).collect();
    Ok(Source::Pixels(window, px))
}

/// How the source goes onto the target.
#[derive(Clone, Copy)]
struct Blend {
    mode: BlendMode,
    opacity: f32,
    /// Leave every pixel's alpha as it was (Preserve Transparency, or locked transparency).
    keep_alpha: bool,
    /// Normal mode replaces the pixels, transparency included (History: back to that state).
    restore: bool,
}

/// `d` moved towards `c` by `k`, in premultiplied space (straight RGBA in and out).
fn lerp(d: [f32; 4], c: [f32; 4], k: f32) -> [f32; 4] {
    let a = d[3] * (1.0 - k) + c[3] * k;
    if a <= 0.0 {
        return [0.0; 4];
    }
    let ch = |i: usize| (d[i] * d[3] * (1.0 - k) + c[i] * c[3] * k) / a;
    [ch(0), ch(1), ch(2), a]
}

/// Blend `source` into `surf` over `area`: per pixel, the selection coverage times the opacity,
/// in the blend mode.
/// Works tile by tile in parallel on the encoded pixels (no full-surface f32 round trip).
fn blend_into(surf: &mut Surface, area: Rect, source: &Source, sel: Option<&Surface>, b: Blend) {
    use rayon::prelude::*;
    let fmt = surf.format();
    // An opaque colour in Normal mode at 100 % that may replace alpha: fully covered tiles become
    // one shared solid tile (copy-on-write), which costs nothing per pixel.
    let solid = match source {
        Source::Color(c) if c[3] >= 1.0 && b.mode == BlendMode::Normal && b.opacity >= 1.0 && (!b.keep_alpha || !fmt.alpha) => {
            let mut enc = [0.0f32; 8];
            let n = from_rgba_into(&fmt, *c, &mut enc);
            Some(surf.solid_tile(enc.get(..n).unwrap_or(&[])))
        }
        _ => None,
    };
    let tiles = surf.take_tiles(area);
    let done: Vec<_> = tiles
        .into_par_iter()
        .map(|(tc, tile)| {
            let tr = tc.rect().intersect(&area);
            let mask = sel.map(|m| Mask { bytes: m.tile(tc).map_or(m.default_bytes(), |t| t.bytes()), fmt: m.format(), origin: tc.rect() });
            if let Some(s) = &solid
                && tr == tc.rect()
                && mask.as_ref().is_none_or(|m| m.full(tr))
            {
                return (tc, s.clone());
            }
            let mut tile = std::sync::Arc::unwrap_or_clone(tile);
            blend_tile(tile.bytes_mut(), &fmt, tc.rect(), tr, source, mask.as_ref(), b);
            (tc, std::sync::Arc::new(tile))
        })
        .collect();
    surf.put_tiles(done);
}

/// The selection's coverage over one tile (its channel 0).
struct Mask<'a> {
    bytes: &'a [u8],
    fmt: photocraft_color::PixelFormat,
    origin: Rect,
}

impl Mask<'_> {
    fn at(&self, x: i32, y: i32) -> f32 {
        let bpp = self.fmt.bytes_per_pixel();
        // A default-pixel mask is one pixel long: every position reads it.
        let i = if self.bytes.len() == bpp { 0 } else { ((y - self.origin.y0) as usize * TILE_SIZE as usize + (x - self.origin.x0) as usize) * bpp };
        self.bytes.get(i..i + bpp).map_or(0.0, |px| photocraft_color::read_sample(px, self.fmt.sample, 0))
    }
    /// Fully selected over `r`?
    fn full(&self, r: Rect) -> bool {
        (r.y0..r.y1).all(|y| (r.x0..r.x1).all(|x| self.at(x, y) >= 1.0))
    }
}

/// Blend into one tile's encoded pixels (`bytes`, the tile at `origin`) over `tr`.
fn blend_tile(bytes: &mut [u8], fmt: &photocraft_color::PixelFormat, origin: Rect, tr: Rect, source: &Source, mask: Option<&Mask>, b: Blend) {
    let Blend { mode, opacity, keep_alpha, restore } = b;
    let n = fmt.channels();
    let bpp = fmt.bytes_per_pixel();
    let blend = if mode == BlendMode::Dissolve { BlendMode::Normal } else { mode };
    let mut px = [0.0f32; 8];
    let mut enc = [0.0f32; 8];
    // An opaque colour at full coverage in Normal mode just replaces the colour (and the alpha
    // unless it is kept): copy its encoded bytes.
    let opaque = match source {
        Source::Color(c) if c[3] >= 1.0 && mode == BlendMode::Normal => {
            let m = from_rgba_into(fmt, *c, &mut enc);
            let mut bytes = vec![0u8; bpp];
            photocraft_raster::encode_pixel(fmt, enc.get(..m.min(n)).unwrap_or(&[]), &mut bytes);
            // Keeping alpha: copy the colour samples only (alpha is the last sample).
            let keep = if (keep_alpha || !fmt.alpha) && fmt.alpha { bpp - bpp / n.max(1) } else { bpp };
            Some((bytes, keep))
        }
        _ => None,
    };
    for y in tr.y0..tr.y1 {
        let row = (y - origin.y0) as usize * TILE_SIZE as usize;
        for x in tr.x0..tr.x1 {
            let mut k = mask.map_or(1.0, |m| m.at(x, y)) * opacity;
            if k <= 0.0 {
                continue;
            }
            let i = (row + (x - origin.x0) as usize) * bpp;
            let Some(pb) = bytes.get_mut(i..i + bpp) else { continue };
            if k >= 1.0
                && let Some((enc_px, keep)) = &opaque
            {
                if let (Some(dst), Some(src)) = (pb.get_mut(..*keep), enc_px.get(..*keep)) {
                    dst.copy_from_slice(src);
                }
                continue;
            }
            for (c, v) in px.iter_mut().enumerate().take(n) {
                *v = photocraft_color::read_sample(pb, fmt.sample, c);
            }
            let d = to_rgba(fmt, &px[..n.min(8)]);
            let mut c = source.at(x, y);
            if mode == BlendMode::Dissolve {
                // Dissolve: each pixel is either fully painted or untouched, with the coverage as odds.
                k = if photocraft_color::dither_noise(x, y) < c[3] * k { 1.0 } else { 0.0 };
                c[3] = 1.0;
                if k <= 0.0 {
                    continue;
                }
            }
            let mut o = if restore && blend == BlendMode::Normal { lerp(d, c, k) } else { photocraft_color::blend::composite(blend, d, c, k) };
            if keep_alpha || !fmt.alpha {
                o[3] = d[3];
            }
            let m = from_rgba_into(fmt, o, &mut enc);
            photocraft_raster::encode_pixel(fmt, enc.get(..m.min(n)).unwrap_or(&[]), pb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 30, "background": "transparent"})).unwrap();
        s
    }

    fn px(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let st = s.active().unwrap();
        let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
        l.surface().unwrap().rgba(x, y)
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1.5 / 255.0)
    }

    #[test]
    fn every_constant_contents() {
        let mut s = session();
        s.execute("tools.setColors", json!({"foreground": [1.0, 0.0, 0.0, 1.0], "background": [0.0, 0.0, 1.0, 1.0]})).unwrap();
        for (c, want) in [
            ("foreground", [1.0, 0.0, 0.0, 1.0]),
            ("background", [0.0, 0.0, 1.0, 1.0]),
            ("black", [0.0, 0.0, 0.0, 1.0]),
            ("gray", [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0]),
            ("white", [1.0; 4]),
        ] {
            s.execute(CMD, json!({"contents": c})).unwrap();
            assert!(close(px(&s, 5, 5), want), "{c}: {:?}", px(&s, 5, 5));
        }
        s.execute(CMD, json!({"contents": "color", "color": "#00ff00"})).unwrap();
        assert!(close(px(&s, 5, 5), [0.0, 1.0, 0.0, 1.0]));
        // The old form: a colour, or the foreground.
        s.execute(CMD, json!({"color": "#ffff00"})).unwrap();
        assert!(close(px(&s, 5, 5), [1.0, 1.0, 0.0, 1.0]));
    }

    #[test]
    fn opacity_mode_and_preserve_transparency() {
        let mut s = session();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 20, "height": 30})).unwrap();
        s.execute(CMD, json!({"contents": "white"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        // Preserve Transparency: the empty right half stays empty.
        s.execute(CMD, json!({"contents": "black", "opacity": 50, "preserveTransparency": true})).unwrap();
        assert!(close(px(&s, 5, 5), [0.5, 0.5, 0.5, 1.0]), "{:?}", px(&s, 5, 5));
        assert_eq!(px(&s, 30, 5)[3], 0.0);
        // Multiply with white leaves the grey.
        s.execute(CMD, json!({"contents": "white", "mode": "multiply", "preserveTransparency": true})).unwrap();
        assert!(close(px(&s, 5, 5), [0.5, 0.5, 0.5, 1.0]));
        // Without it, the transparent half is filled.
        s.execute(CMD, json!({"contents": "white", "opacity": 100})).unwrap();
        assert_eq!(px(&s, 30, 5), [1.0; 4]);
        // Dissolve paints whole pixels only.
        s.execute(CMD, json!({"contents": "black", "mode": "dissolve", "opacity": 50})).unwrap();
        let st = s.active().unwrap();
        let surf = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap();
        let blacks = (0..40).flat_map(|x| (0..30).map(move |y| (x, y))).filter(|&(x, y)| surf.rgba(x, y)[0] < 0.01).count();
        assert!(blacks > 300 && blacks < 900, "{blacks}");
        assert!((0..40).all(|x| {
            let v = surf.rgba(x, 3)[0];
            v < 0.01 || v > 0.99
        }));
    }

    #[test]
    fn history_pattern_and_content_aware() {
        let mut s = session();
        s.execute(CMD, json!({"contents": "white"})).unwrap();
        s.execute(CMD, json!({"contents": "black"})).unwrap();
        // History: back to state 1 (after the white fill).
        s.execute(CMD, json!({"contents": "history", "state": 1})).unwrap();
        assert_eq!(px(&s, 5, 5), [1.0; 4]);
        // The default source is the oldest state: the empty layer.
        s.execute(CMD, json!({"contents": "history"})).unwrap();
        assert_eq!(px(&s, 5, 5)[3], 0.0);
        assert!(s.execute(CMD, json!({"contents": "history", "state": 999})).is_err());
        let r = s.execute(CMD, json!({"contents": "pattern", "pattern": "Diagonal Lines"})).unwrap();
        assert!(r["pattern"].is_string());
        assert!(px(&s, 5, 5)[3] > 0.0);
        // Content-Aware fills the hole from what is around it.
        s.execute(CMD, json!({"contents": "color", "color": "#336699"})).unwrap();
        s.execute("select.rect", json!({"x": 15, "y": 10, "width": 6, "height": 6})).unwrap();
        s.execute(CMD, json!({"contents": "white"})).unwrap();
        s.execute(CMD, json!({"contents": "contentAware", "colorAdaptation": false})).unwrap();
        assert!(close(px(&s, 17, 12), [0x33 as f32 / 255.0, 0x66 as f32 / 255.0, 0x99 as f32 / 255.0, 1.0]), "{:?}", px(&s, 17, 12));
    }

    /// `cargo test -p photocraft-engine --release --lib fill_cmds::tests::timing_24mp -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn timing_24mp() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 6000, "height": 4000, "background": "white"})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute(CMD, json!({"contents": "white"})).unwrap();
        let time = |s: &mut Session, label: &str, p: Value| {
            let mut v = Vec::new();
            for i in 0..5 {
                let mut p = p.clone();
                if p.get("contents").is_none() {
                    p["color"] = json!(if i % 2 == 0 { "#336699" } else { "#996633" });
                }
                let t = std::time::Instant::now();
                s.execute(CMD, p).unwrap();
                v.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            v.sort_by(f64::total_cmp);
            println!("{label:<48} median {:>8.1} ms  min {:>8.1} ms", v[2], v[0]);
        };
        time(&mut s, "colour, no selection", json!({}));
        time(&mut s, "colour, preserve transparency", json!({"preserveTransparency": true}));
        time(&mut s, "colour 50% multiply", json!({"opacity": 50, "mode": "multiply"}));
        s.execute("select.rect", json!({"x": 100, "y": 100, "width": 5000, "height": 3000})).unwrap();
        time(&mut s, "colour, 15 MP rectangle selection", json!({}));
        s.execute("select.deselect", json!({})).unwrap();
        // The old per-pixel f32 path, for comparison.
        let st = s.active().unwrap();
        let mut surf = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().clone();
        let t = std::time::Instant::now();
        crate::pixels::fill_surface(&mut surf, Rect::new(0, 0, 6000, 4000), [0.2, 0.4, 0.6, 1.0], None, false);
        println!("{:<48} {:>8.1} ms", "old fill_surface, no selection", t.elapsed().as_secs_f64() * 1000.0);
    }

    #[test]
    fn bad_params_are_errors() {
        let mut s = session();
        for p in [
            json!({"contents": "plaid"}),
            json!({"contents": 3}),
            json!({"mode": "sideways"}),
            json!({"mode": "passThrough"}),
            json!({"opacity": "lots"}),
            json!({"preserveTransparency": "yes"}),
            json!({"contents": "contentAware"}),
            json!({"contents": "pattern", "pattern": "no such pattern"}),
            json!({"contents": "history", "state": -1}),
            json!({"contents": "contentAware", "colorAdaptation": 1}),
        ] {
            assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
        }
        // Everything selected leaves nothing to sample from.
        s.execute("select.all", json!({})).unwrap();
        assert!(s.execute(CMD, json!({"contents": "contentAware"})).is_err());
        // Out-of-range opacity is clamped.
        s.execute(CMD, json!({"contents": "white", "opacity": 1e300})).unwrap();
    }
}
