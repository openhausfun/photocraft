//! Retouching tools as commands: Clone Stamp, Healing Brush, Spot Healing Brush, Dodge, Burn,
//! Sponge, Blur, Sharpen, Smudge and History Brush.
//!
//! Every command takes a Photoshop-style brush (`points`, `size`, `hardness`, `opacity`, `flow`,
//! `spacing`, `layer`), respects the active selection as a mask and the layer's transparency lock, and
//! works at any depth (8/16/32f) and colour model. Like the Brush, each paints the targeted surface:
//! the layer's pixels, its mask (`"target":"mask"`), an alpha channel or the Quick Mask (`"target"`,
//! filled in from the Channels panel when absent). The pixel algorithms live in `photocraft-algo`
//! (`poisson`, `inpaint`, `retouch`) and the dab machinery in `photocraft-paint::retouch`; this module
//! only parses parameters and wires them together.

use photocraft_algo::inpaint::{self, CompleteParams};
use photocraft_algo::poisson;
use photocraft_algo::retouch::{ToneRange, dodge_burn, local_blur, local_sharpen, sponge};
use photocraft_color::{BlendMode, PixelFormat, SampleType};
use photocraft_doc::{Document, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_paint::retouch::{Footprint, Region, Smudge, alpha_index, apply_coverage, apply_dab_stroke, stroke_coverage};
use photocraft_paint::{BrushSettings, Stroke, StrokePoint};
use photocraft_raster::{Surface, from_rgba_into, to_rgba};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, blend_from_str};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// A pixel layer, a layer with a mask, or a targeted alpha channel / Quick Mask (as the Brush).
fn has_pixel_layer(s: &Session) -> std::result::Result<(), String> {
    if crate::channel_cmds::edits_channel(s) {
        return Ok(());
    }
    let d = s.active().ok_or("no document open")?;
    let l = d.active_layer.and_then(|id| d.doc.layer(id)).ok_or("no active layer")?;
    if matches!(l.content, LayerContent::Raster(_)) || l.mask.is_some() {
        Ok(())
    } else {
        Err(format!("active layer is a {} layer, not a pixel layer", l.content.kind_name()))
    }
}

/// The stroke paints layer pixels (not a mask or channel), so other layers can be sampled.
fn targets_pixels(p: &Value) -> bool {
    crate::channel_cmds::target_of(p) == crate::channel_cmds::Target::Pixels
}

fn num(p: &Value, k: &str, d: f32) -> f32 {
    p.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}
fn flag(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn string<'a>(p: &'a Value, k: &str, d: &'a str) -> &'a str {
    p.get(k).and_then(Value::as_str).unwrap_or(d)
}
fn point(p: &Value, k: &str) -> Option<(f64, f64)> {
    let a = p.get(k)?.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

/// Parse the shared brush params into a stroke on a layer (`None` when an alpha channel or the
/// Quick Mask is targeted).
fn parse_brush(s: &Session, p: &Value, cmd: &str) -> Result<(Stroke, Option<LayerId>)> {
    let pts: Vec<StrokePoint> = p
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(cmd, "missing `points`"))?
        .iter()
        .filter_map(|v| {
            let a = v.as_array()?;
            Some(StrokePoint::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2).and_then(Value::as_f64).unwrap_or(1.0) as f32))
        })
        .collect();
    if pts.is_empty() {
        return Err(bad(cmd, "`points` is empty"));
    }
    let base = s.tools.brush.clone();
    let pct = |k: &str, d: f32, lo: f32, hi: f32| num(p, k, d).clamp(lo, hi) / 100.0;
    let brush = BrushSettings {
        size: num(p, "size", base.size).max(1.0),
        hardness: pct("hardness", base.hardness * 100.0, 0.0, 100.0),
        opacity: pct("opacity", 100.0, 1.0, 100.0),
        flow: pct("flow", 100.0, 1.0, 100.0),
        spacing: pct("spacing", 25.0, 1.0, 1000.0),
        erase: false,
        ..base
    };
    if crate::channel_cmds::is_channel_target(p) {
        return Ok((Stroke { brush, points: pts }, None));
    }
    let id = match p.get("layer").and_then(Value::as_u64) {
        Some(id) => LayerId(id),
        None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into()))?,
    };
    Ok((Stroke { brush, points: pts }, Some(id)))
}

/// Run `f` on the targeted surface (layer pixels, layer mask, alpha channel or Quick Mask: the
/// Brush's paint target) inside one undoable step. `f` gets the pre-stroke document (a cheap
/// copy-on-write snapshot), the surface, the selection and the transparency lock.
fn run_stroke(
    s: &mut Session,
    label: &str,
    id: Option<LayerId>,
    p: &Value,
    f: impl FnOnce(&Document, &mut Surface, Option<&Surface>, bool) -> Result<Rect>,
) -> Result<Rect> {
    let dmg = s.edit(label, |doc, _| {
        let pre = doc.clone();
        let (surf, lock) = crate::channel_cmds::target_surface(doc, id, p)?;
        // No prune: retouching only rewrites pixels inside the stroke, and scanning every tile of a
        // large layer would dominate the cost of a dab.
        f(&pre, surf, pre.selection.as_ref(), lock)
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(dmg);
    }
    Ok(dmg)
}

fn damage_json(r: Rect) -> Value {
    json!([r.x0, r.y0, r.width(), r.height()])
}

// ---------------------------------------------------------------------------------------------
// Sampling sources
// ---------------------------------------------------------------------------------------------

/// Which pixels Clone Stamp / Healing Brush sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleLayers {
    Current,
    CurrentAndBelow,
    All,
}

fn sample_layers(p: &Value, cmd: &str) -> Result<SampleLayers> {
    match string(p, "sampleLayer", "current") {
        "current" => Ok(SampleLayers::Current),
        "currentAndBelow" => Ok(SampleLayers::CurrentAndBelow),
        "all" => Ok(SampleLayers::All),
        o => Err(bad(cmd, format!("unknown sampleLayer `{o}` (current|currentAndBelow|all)"))),
    }
}

/// Composite pixels of `rect` in `fmt` (native channels): the whole document, or the target layer
/// and everything below it.
fn composite_region(pre: &Document, id: Option<LayerId>, which: SampleLayers, rect: Rect, fmt: PixelFormat) -> Region {
    let mut doc = pre.clone();
    if which == SampleLayers::CurrentAndBelow
        && let Some(id) = id
    {
        // Hide everything composited after (above) the target in the bottom-to-top walk.
        let walk: Vec<(Vec<usize>, LayerId)> = pre.walk().into_iter().map(|(p, _, l)| (p, l.id)).collect();
        if let Some(pos) = walk.iter().position(|(_, l)| *l == id) {
            for (path, _) in &walk[pos + 1..] {
                if let Some(l) = doc.layer_at_mut(path) {
                    l.visible = false;
                }
            }
        }
    }
    let buf = photocraft_compose::render(&doc, rect);
    let n = fmt.channels();
    let mut out = Region::new(rect, n);
    let mut enc = [0.0f32; 8];
    for (i, px) in buf.px.iter().enumerate() {
        from_rgba_into(&fmt, *px, &mut enc);
        out.data[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
    }
    out
}

/// Pixels to sample at `rect` (document coordinates, already offset to the source position).
fn sample(pre: &Document, id: Option<LayerId>, surf: &Surface, which: SampleLayers, rect: Rect) -> Region {
    match which {
        SampleLayers::Current => Region::read(surf, rect),
        _ => composite_region(pre, id, which, rect, surf.format()),
    }
}

/// Where a clone stroke samples: `offset` / `source` params, else the active Clone Source slot,
/// with the slot's (or the call's) scale, rotation and flips (see `presets::clone_source`).
fn clone_mapping(s: &mut Session, p: &Value, stroke: &Stroke, cmd: &str) -> Result<crate::presets::clone_source::Mapping> {
    let f = stroke.points[0];
    crate::presets::clone_source::mapping(s, p, (f.x, f.y), cmd)
}

/// Source pixels for destination `rect`: a translated read, or a bilinear resample when the
/// clone source is scaled, rotated or flipped.
fn clone_sample(pre: &Document, id: Option<LayerId>, surf: &Surface, which: SampleLayers, rect: Rect, map: &crate::presets::clone_source::Mapping) -> Region {
    if map.is_translation() {
        let off = map.offset();
        let mut r = sample(pre, id, surf, which, rect.translate(off.0, off.1));
        r.rect = rect;
        return r;
    }
    let src = sample(pre, id, surf, which, map.source_rect(rect));
    crate::presets::clone_source::resample(&src, rect, map, alpha_index(&surf.format()))
}

/// Result JSON shared by Clone Stamp and Healing Brush: the offset used and where the next stroke
/// should sample from if the UI keeps Photoshop's "Aligned" semantics.
fn clone_result(dmg: Rect, off: (i32, i32), aligned: bool, p: &Value, stroke: &Stroke) -> Value {
    let f = stroke.points[0];
    let source = point(p, "source").unwrap_or((f.x + off.0 as f64, f.y + off.1 as f64));
    // Aligned: the offset persists (next stroke samples at its own first point + offset).
    // Non-aligned: every stroke restarts at the original source point.
    json!({ "damage": damage_json(dmg), "offset": [off.0, off.1], "aligned": aligned, "nextSource": if aligned { Value::Null } else { json!([source.0, source.1]) } })
}

fn blend_param(p: &Value, cmd: &str) -> Result<BlendMode> {
    match p.get("mode").and_then(Value::as_str) {
        None => Ok(BlendMode::Normal),
        Some(m) => blend_from_str(m).ok_or_else(|| bad(cmd, format!("unknown blend mode `{m}`"))),
    }
}

/// Clamp results into the storable range: integer depths to 0..1, float depths only below at 0;
/// alpha always 0..1.
fn clamp_samples(fmt: &PixelFormat, data: &mut [f32]) {
    let n = fmt.channels();
    let a = alpha_index(fmt);
    let float = fmt.sample == SampleType::F32;
    for px in data.chunks_exact_mut(n) {
        for (c, v) in px.iter_mut().enumerate() {
            *v = if Some(c) == a || !float { v.clamp(0.0, 1.0) } else { v.max(0.0) };
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Clone Stamp / Healing Brush / History Brush
// ---------------------------------------------------------------------------------------------

fn clone_stamp(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "paint.cloneStamp";
    let (stroke, id) = parse_brush(s, p, CMD)?;
    let which = sample_layers(p, CMD)?;
    let mode = blend_param(p, CMD)?;
    let aligned = flag(p, "aligned", true);
    let map = clone_mapping(s, p, &stroke, CMD)?;
    let off = map.offset();
    let dmg = run_stroke(s, "Clone Stamp", id, p, |pre, surf, sel, lock| {
        let (bounds, cov) = stroke_coverage(&stroke);
        // Samples come from the pre-stroke state, so pixels painted earlier in the stroke are never re-cloned.
        let paint = clone_sample(pre, id, surf, which, bounds, &map);
        Ok(apply_coverage(surf, bounds, &cov, stroke.brush.opacity, sel, lock, &paint, mode))
    })?;
    Ok(clone_result(dmg, off, aligned, p, &stroke))
}

/// Grow a stroke's coverage map by `m` pixels of zero coverage (a Dirichlet boundary for the solve).
fn pad_coverage(bounds: Rect, cov: &[f32], m: i32) -> (Rect, Vec<f32>) {
    let g = bounds.inflate(m);
    let (w, gw) = (bounds.width() as usize, g.width() as usize);
    let mut out = vec![0.0f32; gw * g.height() as usize];
    for (i, c) in cov.iter().enumerate() {
        let (x, y) = (i % w + m as usize, i / w + m as usize);
        out[y * gw + x] = *c;
    }
    (g, out)
}

/// Seamless-clone `src` into `dst` over the mask (both regions cover the same rect). The solve runs
/// only on the mask's bounding box plus a one-pixel Dirichlet ring; the rest is `dst`.
fn heal_region(fmt: &PixelFormat, src: &Region, dst: &Region, mask: &[bool]) -> Region {
    let w = dst.width();
    let bbox = mask.iter().enumerate().filter(|(_, m)| **m).fold(Rect::EMPTY, |r, (i, _)| {
        let (x, y) = (dst.rect.x0 + (i % w) as i32, dst.rect.y0 + (i / w) as i32);
        r.union(&Rect::new(x, y, x + 1, y + 1))
    });
    let mut out = dst.clone();
    if bbox.is_empty() {
        return out;
    }
    let r = bbox.inflate(1).intersect(&dst.rect);
    let (s, d) = (src.crop(r), dst.crop(r));
    let m: Vec<bool> = (r.y0..r.y1).flat_map(|y| (r.x0..r.x1).map(move |x| (x, y))).map(|(x, y)| mask[dst.index(x, y)]).collect();
    let mut data = poisson::seamless_clone(d.width(), d.height(), d.ch, &s.data, &d.data, &m);
    clamp_samples(fmt, &mut data);
    let (n, rw) = (dst.ch, r.width() as usize);
    for y in r.y0..r.y1 {
        let o = out.index(r.x0, y) * n;
        let i = (y - r.y0) as usize * rw * n;
        out.data[o..o + rw * n].copy_from_slice(&data[i..i + rw * n]);
    }
    out
}

fn healing_brush(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "paint.healingBrush";
    let (stroke, id) = parse_brush(s, p, CMD)?;
    let which = sample_layers(p, CMD)?;
    let mode = blend_param(p, CMD)?;
    let aligned = flag(p, "aligned", true);
    let map = clone_mapping(s, p, &stroke, CMD)?;
    let off = map.offset();
    let dmg = run_stroke(s, "Healing Brush", id, p, |pre, surf, sel, lock| {
        let (bounds, cov) = stroke_coverage(&stroke);
        let (g, cov) = pad_coverage(bounds, &cov, 2);
        let fmt = surf.format();
        let src = clone_sample(pre, id, surf, which, g, &map);
        // The destination the texture is fitted to: the layer itself, or what the user sees when
        // sampling several layers (so healing onto an empty layer matches the composite).
        let dst = if which == SampleLayers::Current { Region::read(surf, g) } else { composite_region(pre, id, which, g, fmt) };
        let mask: Vec<bool> = cov.iter().map(|c| *c > 0.0).collect();
        let healed = heal_region(&fmt, &src, &dst, &mask);
        Ok(apply_coverage(surf, g, &cov, stroke.brush.opacity, sel, lock, &healed, mode))
    })?;
    Ok(clone_result(dmg, off, aligned, p, &stroke))
}

fn history_brush(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "paint.historyBrush";
    let (stroke, id) = parse_brush(s, p, CMD)?;
    let mode = blend_param(p, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let explicit = p.get("state").and_then(Value::as_u64).map(|v| v as usize);
    let source_doc = match explicit {
        Some(i) if i == st.history.past_len() => st.doc.clone(),
        Some(i) => st.history.state(i).ok_or_else(|| bad(CMD, format!("no history state {i} (0..={})", st.history.past_len())))?,
        // Default: the oldest state still held (the "Open" snapshot unless it was trimmed).
        None => st.history.state(0).unwrap_or_else(|| st.doc.clone()),
    };
    // The same target (pixels, mask, channel) as it was in that state.
    let mut source_doc = (*source_doc).clone();
    let src_surface = crate::channel_cmds::target_surface(&mut source_doc, id, p)
        .map(|(surf, _)| surf.clone())
        .map_err(|_| EngineError::Other("the target did not exist (as pixels, a mask or a channel) in that history state".into()))?;
    let dmg = run_stroke(s, "History Brush", id, p, |_, surf, sel, lock| {
        let fmt = surf.format();
        let src = if src_surface.format() == fmt { src_surface } else { src_surface.convert(fmt) };
        let (bounds, cov) = stroke_coverage(&stroke);
        let paint = Region::read(&src, bounds);
        Ok(apply_coverage(surf, bounds, &cov, stroke.brush.opacity, sel, lock, &paint, mode))
    })?;
    Ok(json!({ "damage": damage_json(dmg) }))
}

// ---------------------------------------------------------------------------------------------
// Spot Healing
// ---------------------------------------------------------------------------------------------

/// Dilate a mask by `r` pixels (square structuring element).
fn dilate(w: usize, h: usize, m: &[bool], r: usize) -> Vec<bool> {
    let mut tmp = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if m[y * w + x] {
                for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                    tmp[y * w + xx] = true;
                }
            }
        }
    }
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if tmp[y * w + x] {
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    out[yy * w + x] = true;
                }
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpotType {
    ContentAware,
    CreateTexture,
    ProximityMatch,
}

/// Fill the stroke area with no source, then gradient-domain blend it into its surroundings.
fn spot_heal_surface(surf: &mut Surface, area: Rect, stroke: &Stroke, kind: SpotType, sel: Option<&Surface>, lock: bool) -> Rect {
    let (bounds, cov) = stroke_coverage(stroke);
    let size = stroke.brush.size;
    let margin = (size.max(8.0) * 1.0).ceil() as i32 + 8;
    let region_rect = bounds.inflate(margin).intersect(&area);
    if region_rect.is_empty() {
        return Rect::EMPTY;
    }
    let fmt = surf.format();
    let img = Region::read(surf, region_rect);
    let (w, h, ch) = (img.width(), img.height(), img.ch);
    let bw = bounds.width() as usize;
    let mut stroke_cov = vec![0.0f32; w * h];
    for (i, c) in cov.iter().enumerate() {
        let (x, y) = (bounds.x0 + (i % bw) as i32, bounds.y0 + (i / bw) as i32);
        if region_rect.contains(x, y) {
            stroke_cov[img.index(x, y)] = *c;
        }
    }
    let hole: Vec<bool> = stroke_cov.iter().map(|c| *c > 0.0).collect();
    if !hole.iter().any(|h| *h) {
        return Rect::EMPTY;
    }
    // Synthesise over a slightly larger domain than the Poisson region, so the solve's boundary pixels
    // carry synthesised values that differ from the untouched surroundings: that mismatch is what the
    // membrane corrects (otherwise the Dirichlet data would be all zero and the blend a no-op).
    let domain = dilate(w, h, &hole, 2);
    let heal_mask = dilate(w, h, &hole, 1);
    let content_aware =
        |img: &[f32]| inpaint::complete(w, h, ch, img, &domain, &CompleteParams::default()).unwrap_or_else(|| poisson::membrane_fill(w, h, ch, img, &domain));
    let filled = match kind {
        SpotType::ContentAware => content_aware(&img.data),
        SpotType::CreateTexture => inpaint::synthesize(w, h, ch, &img.data, &domain, (size / 4.0).clamp(6.0, 32.0) as usize, 0x5eed),
        SpotType::ProximityMatch => {
            let ring = (size / 8.0).clamp(3.0, 16.0) as usize;
            match inpaint::best_offset(w, h, ch, &img.data, &domain, ring, margin) {
                Some((dx, dy)) => {
                    let mut out = img.data.clone();
                    for y in 0..h {
                        for x in 0..w {
                            if domain[y * w + x] {
                                let (i, j) = ((y * w + x) * ch, (((y as i32 + dy) as usize) * w + (x as i32 + dx) as usize) * ch);
                                out.copy_within(j..j + ch, i);
                            }
                        }
                    }
                    out
                }
                None => content_aware(&img.data),
            }
        }
    };
    let src = Region { rect: region_rect, ch, data: filled };
    let healed = heal_region(&fmt, &src, &img, &heal_mask);
    let dmg = bounds.intersect(&region_rect);
    let dmg_cov: Vec<f32> = (dmg.y0..dmg.y1).flat_map(|y| (dmg.x0..dmg.x1).map(move |x| (x, y))).map(|(x, y)| stroke_cov[img.index(x, y)]).collect();
    apply_coverage(surf, dmg, &dmg_cov, stroke.brush.opacity, sel, lock, &healed, BlendMode::Normal)
}

fn spot_healing(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "paint.spotHealing";
    let (stroke, id) = parse_brush(s, p, CMD)?;
    let kind = match string(p, "type", "contentAware") {
        "contentAware" => SpotType::ContentAware,
        "createTexture" => SpotType::CreateTexture,
        "proximityMatch" => SpotType::ProximityMatch,
        o => return Err(bad(CMD, format!("unknown type `{o}` (contentAware|createTexture|proximityMatch)"))),
    };
    let dmg = run_stroke(s, "Spot Healing Brush", id, p, |pre, surf, sel, lock| Ok(spot_heal_surface(surf, pre.bounds(), &stroke, kind, sel, lock)))?;
    Ok(json!({ "damage": damage_json(dmg) }))
}

// ---------------------------------------------------------------------------------------------
// Toning (Dodge / Burn / Sponge) and focus (Blur / Sharpen), Smudge
// ---------------------------------------------------------------------------------------------

/// Per-dab strength such that one pass over a point totals roughly `e`, whatever the spacing
/// (about `1/spacing` dabs overlap each point on the stroke's centre line).
#[inline]
fn per_dab(e: f32, spacing: f32) -> f32 {
    1.0 - (1.0 - e.clamp(0.0, 1.0)).powf(spacing.clamp(0.01, 1.0))
}

/// Apply an RGB colour transform `f(rgb, strength)` to each covered pixel of a dab (through straight
/// RGBA, so it works for Gray/CMYK/Lab too; untouched pixels are never round-tripped).
fn color_dab(fmt: &PixelFormat, work: &mut Region, fp: &Footprint, strength: f32, spacing: f32, f: impl Fn([f32; 3], f32) -> [f32; 3]) {
    let r = fp.rect.intersect(&work.rect);
    let n = fmt.channels();
    let mut enc = [0.0f32; 8];
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let k = per_dab(fp.at(x, y) * strength, spacing);
            if k <= 0.0 {
                continue;
            }
            let px = work.px_mut(x, y);
            let rgba = to_rgba(fmt, px);
            let o = f([rgba[0], rgba[1], rgba[2]], k);
            from_rgba_into(fmt, [o[0], o[1], o[2], rgba[3]], &mut enc);
            let a = alpha_index(fmt);
            for c in 0..n {
                if Some(c) != a {
                    px[c] = enc[c];
                }
            }
        }
    }
}

fn tone_range(p: &Value, cmd: &str) -> Result<ToneRange> {
    match string(p, "range", "midtones") {
        "shadows" => Ok(ToneRange::Shadows),
        "midtones" => Ok(ToneRange::Midtones),
        "highlights" => Ok(ToneRange::Highlights),
        o => Err(bad(cmd, format!("unknown range `{o}` (shadows|midtones|highlights)"))),
    }
}

fn dodge_burn_cmd(s: &mut Session, p: &Value, burn: bool) -> Result<Value> {
    let cmd = if burn { "paint.burn" } else { "paint.dodge" };
    let (stroke, id) = parse_brush(s, p, cmd)?;
    let range = tone_range(p, cmd)?;
    let exposure = num(p, "exposure", 50.0).clamp(1.0, 100.0) / 100.0;
    let protect = flag(p, "protectTones", true);
    let label = if burn { "Burn Tool" } else { "Dodge Tool" };
    let dmg = run_stroke(s, label, id, p, |_, surf, sel, lock| {
        let fmt = surf.format();
        let sp = stroke.brush.spacing;
        Ok(apply_dab_stroke(surf, &stroke, sel, lock, 0, |work, fp| color_dab(&fmt, work, fp, exposure, sp, |c, k| dodge_burn(c, k, range, burn, protect))))
    })?;
    Ok(json!({ "damage": damage_json(dmg) }))
}

fn sponge_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "paint.sponge";
    let (stroke, id) = parse_brush(s, p, CMD)?;
    let saturate = match string(p, "mode", "desaturate") {
        "desaturate" => false,
        "saturate" => true,
        o => return Err(bad(CMD, format!("unknown mode `{o}` (desaturate|saturate)"))),
    };
    let vibrance = flag(p, "vibrance", true);
    let dmg = run_stroke(s, "Sponge Tool", id, p, |_, surf, sel, lock| {
        let fmt = surf.format();
        let sp = stroke.brush.spacing;
        // Flow is already in each dab's coverage; a full-flow pass moves colours half-way.
        Ok(apply_dab_stroke(surf, &stroke, sel, lock, 0, |work, fp| color_dab(&fmt, work, fp, 0.5, sp, |c, k| sponge(c, k, saturate, vibrance))))
    })?;
    Ok(json!({ "damage": damage_json(dmg) }))
}

/// "Sample All Layers" for Blur, Sharpen and Smudge (layer pixels only: a mask or channel has
/// nothing to sample from other layers).
fn sample_all_layers(p: &Value) -> bool {
    flag(p, "sampleAllLayers", false) && targets_pixels(p)
}

/// Sample All Layers for the sequential-dab tools: each dab's effect runs on a copy of the visible
/// composite (kept across dabs, so a dab sees the earlier ones), and the result is laid onto the
/// layer under the dab. An empty layer above the image so picks up the blurred or smudged picture,
/// and pixels the effect didn't change look as before (they are what was already visible).
struct AllLayers<'a> {
    pre: &'a Document,
    fmt: PixelFormat,
    visible: Option<Region>,
}

impl<'a> AllLayers<'a> {
    fn new(pre: &'a Document, fmt: PixelFormat) -> Self {
        Self { pre, fmt, visible: None }
    }

    fn dab(&mut self, work: &mut Region, fp: &Footprint, effect: impl FnOnce(&mut Region, &Footprint)) {
        let (pre, fmt) = (self.pre, self.fmt);
        let visible = self.visible.get_or_insert_with(|| composite_region(pre, None, SampleLayers::All, work.rect, fmt));
        if visible.rect != work.rect || visible.ch != work.ch {
            return effect(work, fp);
        }
        effect(visible, fp);
        let r = fp.rect.intersect(&work.rect);
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                if fp.at(x, y) > 0.0 {
                    work.px_mut(x, y).copy_from_slice(visible.px(x, y));
                }
            }
        }
    }
}

/// Blur (`sharpen = false`) or Sharpen each dab's footprint in the working copy.
fn focus_cmd(s: &mut Session, p: &Value, sharpen: bool) -> Result<Value> {
    let cmd = if sharpen { "paint.sharpen" } else { "paint.blur" };
    let (stroke, id) = parse_brush(s, p, cmd)?;
    let strength = num(p, "strength", 50.0).clamp(1.0, 100.0) / 100.0;
    let protect = flag(p, "protectDetail", true);
    let sigma = (stroke.brush.size * 0.03).clamp(1.0, 4.0);
    let halo = (sigma * 3.0).ceil() as i32 + 1;
    let label = if sharpen { "Sharpen Tool" } else { "Blur Tool" };
    let all = sample_all_layers(p);
    let dmg = run_stroke(s, label, id, p, |pre, surf, sel, lock| {
        let fmt = surf.format();
        let a = alpha_index(&fmt);
        let sp = stroke.brush.spacing;
        let mut sampler = all.then(|| AllLayers::new(pre, fmt));
        let focus = |work: &mut Region, fp: &Footprint| {
            let r = fp.rect.intersect(&work.rect);
            if r.is_empty() {
                return;
            }
            let (w, h, n) = (work.width(), work.height(), work.ch);
            let win = ((r.x0 - work.rect.x0) as usize, (r.y0 - work.rect.y0) as usize, (r.x1 - work.rect.x0) as usize, (r.y1 - work.rect.y0) as usize);
            let out = if sharpen { local_sharpen(&work.data, w, h, n, a, win, 1.0, protect) } else { local_blur(&work.data, w, h, n, a, win, sigma) };
            let rw = r.width() as usize;
            for y in r.y0..r.y1 {
                for x in r.x0..r.x1 {
                    let k = per_dab(fp.at(x, y) * strength, sp);
                    if k <= 0.0 {
                        continue;
                    }
                    let o = ((y - r.y0) as usize * rw + (x - r.x0) as usize) * n;
                    let px = work.px_mut(x, y);
                    for c in 0..n {
                        px[c] += (out[o + c] - px[c]) * k;
                    }
                }
            }
        };
        Ok(apply_dab_stroke(surf, &stroke, sel, lock, halo, |work, fp| match &mut sampler {
            Some(all) => all.dab(work, fp, focus),
            None => focus(work, fp),
        }))
    })?;
    Ok(json!({ "damage": damage_json(dmg) }))
}

fn smudge_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "paint.smudge";
    let (stroke, id) = parse_brush(s, p, CMD)?;
    let strength = num(p, "strength", 50.0).clamp(1.0, 100.0) / 100.0;
    let finger = flag(p, "fingerPainting", false);
    let fg = s.tools.foreground;
    let all = sample_all_layers(p);
    let dmg = run_stroke(s, "Smudge Tool", id, p, |pre, surf, sel, lock| {
        let fmt = surf.format();
        let finger_px = finger.then(|| photocraft_raster::from_rgba(&fmt, fg));
        let max_size = stroke.brush.size;
        // Premultiplied mixing: transparent pixels carry no colour, so no dark fringes.
        let mut sm = Smudge::new(strength, finger_px, max_size).with_alpha(alpha_index(&fmt));
        let mut sampler = all.then(|| AllLayers::new(pre, fmt));
        Ok(apply_dab_stroke(surf, &stroke, sel, lock, 0, |work, fp| match &mut sampler {
            Some(all) => all.dab(work, fp, |w, f| sm.dab(w, f)),
            None => sm.dab(work, fp),
        }))
    })?;
    Ok(json!({ "damage": damage_json(dmg) }))
}

// ---------------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------------

/// The brush every retouching command shares, plus the tool's own params.
macro_rules! brush_params {
    ($extra:literal) => {
        concat!("{", r#""points":[[x,y,pressure?],…],"size":px=tool size,"hardness":0..100=tool hardness,"opacity":1..100=100,"flow":1..100=100,"spacing":1..1000 (% of size)=25,"layer":id?=active,"target":"pixels"|"mask"|"quickMask"|{"channel":i}=Channels panel target"#, $extra, "}")
    };
}

/// Retouching command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "paint.cloneStamp",
            label: "Clone Stamp",
            menu: &[],
            shortcut: None,
            params: brush_params!(
                r#","source":[sx,sy] (sampled under the first point) | "offset":[dx,dy],"aligned":bool=true,"sampleLayer":"current|currentAndBelow|all"="current","mode":"normal|multiply|…"="normal" → {"damage","offset","aligned","nextSource"}"#
            ),
            enabled: has_pixel_layer,
            run: clone_stamp,
            journal: true,
        },
        CommandSpec {
            id: "paint.healingBrush",
            label: "Healing Brush",
            menu: &[],
            shortcut: None,
            params: brush_params!(
                r#","source":[sx,sy] | "offset":[dx,dy],"aligned":bool=true,"sampleLayer":"current|currentAndBelow|all"="current","mode":"normal|…"="normal" → {"damage","offset","aligned","nextSource"}"#
            ),
            enabled: has_pixel_layer,
            run: healing_brush,
            journal: true,
        },
        CommandSpec {
            id: "paint.spotHealing",
            label: "Spot Healing Brush",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","type":"contentAware|createTexture|proximityMatch"="contentAware""#),
            enabled: has_pixel_layer,
            run: spot_healing,
            journal: true,
        },
        CommandSpec {
            id: "paint.dodge",
            label: "Dodge",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","range":"shadows|midtones|highlights"="midtones","exposure":1..100=50,"protectTones":bool=true"#),
            enabled: has_pixel_layer,
            run: |s, p| dodge_burn_cmd(s, p, false),
            journal: true,
        },
        CommandSpec {
            id: "paint.burn",
            label: "Burn",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","range":"shadows|midtones|highlights"="midtones","exposure":1..100=50,"protectTones":bool=true"#),
            enabled: has_pixel_layer,
            run: |s, p| dodge_burn_cmd(s, p, true),
            journal: true,
        },
        CommandSpec {
            id: "paint.sponge",
            label: "Sponge",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","mode":"desaturate|saturate"="desaturate","vibrance":bool=true (flow = sponge strength)"#),
            enabled: has_pixel_layer,
            run: sponge_cmd,
            journal: true,
        },
        CommandSpec {
            id: "paint.blur",
            label: "Blur",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","strength":1..100=50,"sampleAllLayers":bool=false"#),
            enabled: has_pixel_layer,
            run: |s, p| focus_cmd(s, p, false),
            journal: true,
        },
        CommandSpec {
            id: "paint.sharpen",
            label: "Sharpen",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","strength":1..100=50,"protectDetail":bool=true,"sampleAllLayers":bool=false"#),
            enabled: has_pixel_layer,
            run: |s, p| focus_cmd(s, p, true),
            journal: true,
        },
        CommandSpec {
            id: "paint.smudge",
            label: "Smudge",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","strength":1..100=50,"fingerPainting":bool=false (starts with the foreground colour),"sampleAllLayers":bool=false"#),
            enabled: has_pixel_layer,
            run: smudge_cmd,
            journal: true,
        },
        CommandSpec {
            id: "paint.historyBrush",
            label: "History Brush",
            menu: &[],
            shortcut: None,
            params: brush_params!(r#","state":index (into history.entries; default 0 = the oldest held state, normally the Open snapshot)"#),
            enabled: has_pixel_layer,
            run: history_brush,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests;
