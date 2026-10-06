//! Everyday Photoshop commands that build on the core ones: Edit › Stroke, the fixed Transform
//! presets (Rotate 180°/90°, Flip), Paste Into, Reselect, Equalize, Reveal All, Layer from
//! Background, Copy/Paste Layer Style, Hide All Effects, layer-mask toggles, Rasterize variants,
//! Delete Hidden / Empty Layers, Ungroup, Hide/Show Layers, Average and Clouds.

use photocraft_algo::selection as sel;
use photocraft_doc::{Document, Layer, LayerContent, LayerId, LayerMask};
use photocraft_geom::Rect;
use photocraft_raster::{from_rgba_into, to_rgba};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, color_param, layer_param};
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn active_layer(s: &Session) -> std::result::Result<&Layer, String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.and_then(|id| d.doc.layer(id)).ok_or_else(|| "no active layer".into())
}

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    active_layer(s).map(|_| ())
}

fn has_pixels(s: &Session) -> std::result::Result<(), String> {
    match active_layer(s)?.content {
        LayerContent::Raster(_) => Ok(()),
        ref c => Err(format!("active layer is a {} layer, not a pixel layer", c.kind_name())),
    }
}

fn has_mask(s: &Session) -> std::result::Result<(), String> {
    active_layer(s)?.mask.as_ref().map(|_| ()).ok_or_else(|| "the active layer has no layer mask".into())
}

fn has_clip_and_selection(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    s.clipboard.as_ref().ok_or("the clipboard is empty")?;
    d.doc.selection.as_ref().map(|_| ()).ok_or_else(|| "Paste Into needs a selection".into())
}

pub(crate) fn is_background(l: &Layer) -> bool {
    l.name == "Background" && l.locks.transparency && l.locks.position && matches!(l.content, LayerContent::Raster(_))
}

fn has_background(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    d.doc.layers.first().filter(|l| is_background(l)).map(|_| ()).ok_or_else(|| "the document has no Background layer".into())
}

/// A key that makes the sub-commands of one command share a single history step.
fn step_key(s: &Session, what: &str) -> String {
    format!("{what}#{}", s.active().map_or(0, |d| d.revision))
}

// ---------- pixel helpers ----------

/// Rewrite the active pixel layer's RGBA over the selection bounds (or the canvas). `f` gets the
/// area, the pixels, and the selection coverage per pixel; results are blended back by coverage,
/// and transparency stays locked when the layer locks it.
fn map_pixels(s: &mut Session, label: &str, f: impl FnOnce(Rect, &mut [[f32; 4]], &[f32])) -> Result<Value> {
    let id = layer_param(s, &Value::Null)?;
    s.edit(label, |doc, _| {
        let canvas = doc.bounds();
        let selection = doc.selection.clone();
        let area = selection.as_ref().map_or(canvas, |m| m.content_bounds().intersect(&canvas));
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.locks.pixels || l.locks.all {
            return Err(EngineError::Other(format!("Could not complete your request because the layer \"{}\" is locked", l.name)));
        }
        let lock_alpha = l.locks.transparency;
        let surf = l.surface_mut().ok_or_else(|| EngineError::Other("not a pixel layer".into()))?;
        if area.is_empty() {
            return Ok(());
        }
        let fmt = surf.format();
        let n = fmt.channels();
        let mut raw = surf.read_region(area);
        let orig: Vec<[f32; 4]> = raw.chunks_exact(n).map(|p| to_rgba(&fmt, p)).collect();
        let w = area.width() as usize;
        let cover: Vec<f32> = match &selection {
            Some(m) => (0..orig.len()).map(|i| m.sample_channel(area.x0 + (i % w) as i32, area.y0 + (i / w) as i32, 0)).collect(),
            None => vec![1.0; orig.len()],
        };
        let mut px = orig.clone();
        f(area, &mut px, &cover);
        let mut enc = [0.0f32; 8];
        for (i, out) in raw.chunks_exact_mut(n).enumerate() {
            let k = cover[i];
            if k <= 0.0 {
                continue;
            }
            let (o, p) = (orig[i], px[i]);
            let mut v = [0.0; 4];
            for c in 0..4 {
                v[c] = o[c] + (p[c] - o[c]) * k;
            }
            if lock_alpha {
                v[3] = o[3];
            }
            from_rgba_into(&fmt, v, &mut enc);
            out.copy_from_slice(&enc[..n]);
        }
        surf.write_region(area, &raw);
        surf.prune();
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Edit › Stroke: a band along the selection edge (or the layer's opaque edge without a selection).
fn stroke(s: &mut Session, p: &Value) -> Result<Value> {
    let width = p.get("width").and_then(Value::as_f64).unwrap_or(1.0).clamp(1.0, 250.0) as f32;
    let mut color = color_param(p, "color", s.tools.foreground);
    color[3] *= (p.get("opacity").and_then(Value::as_f64).unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0);
    let location = p.get("location").and_then(Value::as_str).unwrap_or("center").to_string();
    let id = layer_param(s, p)?;
    s.edit("Stroke", |doc, _| {
        let canvas = doc.bounds();
        let (w, h) = (canvas.width() as usize, canvas.height() as usize);
        let edge = match &doc.selection {
            Some(m) => sel::mask_from_surface(Some(m), canvas),
            None => {
                let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
                let surf = l.surface().ok_or_else(|| EngineError::Other("not a pixel layer".into()))?;
                let mut px = vec![[0.0f32; 4]; w * h];
                surf.read_rgba_into(canvas, &mut px);
                px.iter().map(|p| p[3]).collect()
            }
        };
        let (outer, inner) = match location.as_str() {
            "inside" => (edge.clone(), sel::contract(&edge, w, h, width)),
            "outside" => (sel::expand(&edge, w, h, width), edge.clone()),
            _ => (sel::expand(&edge, w, h, width / 2.0), sel::contract(&edge, w, h, width / 2.0)),
        };
        let band: Vec<f32> = outer.iter().zip(&inner).map(|(o, i)| (o - i).clamp(0.0, 1.0)).collect();
        let band = sel::mask_to_surface(&band, canvas);
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let lock = l.locks.transparency;
        let surf = crate::commands::paint_surface(l, &Value::Null)?;
        let area = band.content_bounds().intersect(&canvas);
        if !area.is_empty() {
            crate::pixels::fill_surface(surf, area, color, Some(&band), lock);
            surf.prune();
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Histogram equalization, like Photoshop's: one CDF over the R, G and B values of
/// the selected pixels, applied to every channel.
fn equalize(s: &mut Session) -> Result<Value> {
    map_pixels(s, "Equalize", |_, px, cover| {
        let mut hist = [0f64; 256];
        for (p, &k) in px.iter().zip(cover) {
            if k > 0.0 && p[3] > 0.0 {
                for &v in &p[..3] {
                    hist[(v.clamp(0.0, 1.0) * 255.0).round() as usize] += 1.0;
                }
            }
        }
        let total: f64 = hist.iter().sum();
        if total == 0.0 {
            return;
        }
        let first = hist.iter().copied().find(|v| *v > 0.0).unwrap_or(0.0);
        let mut lut = [0f32; 256];
        let mut acc = 0.0;
        for (i, h) in hist.iter().enumerate() {
            acc += h;
            lut[i] = if total > first { ((acc - first) / (total - first)).clamp(0.0, 1.0) as f32 } else { i as f32 / 255.0 };
        }
        // Same 256 bins as the histogram, at every depth (like Photoshop).
        let map = |v: f32| lut[(v.clamp(0.0, 1.0) * 255.0).round() as usize];
        for p in px.iter_mut() {
            for v in &mut p[..3] {
                *v = map(*v);
            }
        }
    })
}

/// Filter › Blur › Average: the selection (or layer) filled with its mean colour.
fn average(s: &mut Session) -> Result<Value> {
    map_pixels(s, "Average", |_, px, cover| {
        let (mut sum, mut wsum, mut asum, mut n) = ([0f64; 3], 0f64, 0f64, 0f64);
        for (p, &k) in px.iter().zip(cover) {
            let w = (p[3] * k) as f64;
            for c in 0..3 {
                sum[c] += p[c] as f64 * w;
            }
            wsum += w;
            asum += (p[3] * k) as f64;
            n += k as f64;
        }
        if wsum <= 0.0 {
            return;
        }
        let avg = [(sum[0] / wsum) as f32, (sum[1] / wsum) as f32, (sum[2] / wsum) as f32, (asum / n.max(1e-9)) as f32];
        px.fill(avg);
    })
}

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 0x00ff_ffff as f32
}

/// Smooth value noise, summed over octaves (fBm), in 0..1.
pub(crate) fn clouds_value(x: f32, y: f32, base: f32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0 / base, 0.0);
    for o in 0..8u32 {
        let (fx, fy) = (x * freq, y * freq);
        let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
        let s = seed.wrapping_add(o * 7919);
        let top = hash(ix, iy, s) + (hash(ix + 1, iy, s) - hash(ix, iy, s)) * sx;
        let bot = hash(ix, iy + 1, s) + (hash(ix + 1, iy + 1, s) - hash(ix, iy + 1, s)) * sx;
        sum += (top + (bot - top) * sy) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    // Stretch the bell-shaped sum so the clouds use the full foreground→background range.
    ((sum / norm - 0.5) * 2.2 + 0.5).clamp(0.0, 1.0)
}

/// Filter › Render › Clouds / Difference Clouds, between the foreground and background colours.
fn clouds(s: &mut Session, p: &Value, difference: bool) -> Result<Value> {
    let (fg, bg) = (s.tools.foreground, s.tools.background);
    let seed = p.get("seed").and_then(Value::as_u64).unwrap_or(0) as u32;
    let size = s.active().map_or(256.0, |d| d.doc.size.width.max(d.doc.size.height) as f32);
    let base = (size / 4.0).clamp(16.0, 512.0);
    map_pixels(s, if difference { "Difference Clouds" } else { "Clouds" }, |area, px, _| {
        let w = area.width() as usize;
        for (i, p) in px.iter_mut().enumerate() {
            let (x, y) = ((area.x0 + (i % w) as i32) as f32, (area.y0 + (i / w) as i32) as f32);
            let t = clouds_value(x, y, base, seed);
            let c: [f32; 3] = std::array::from_fn(|k| fg[k] + (bg[k] - fg[k]) * t);
            if difference {
                for k in 0..3 {
                    p[k] = (p[k] - c[k]).abs();
                }
            } else {
                *p = [c[0], c[1], c[2], 1.0];
            }
        }
    })
}

// ---------- transform presets ----------

fn transform_preset(s: &mut Session, kind: &str) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let id = d.active_layer.ok_or(EngineError::Other("no active layer".into()))?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let b = crate::transform_cmds::transform_bounds(&d.doc, l);
    if b.is_empty() {
        return Err(EngineError::Other("nothing to transform".into()));
    }
    // Integer offsets keep these exact pixel permutations on the grid (nearest sampling).
    let (sx, sy) = ((b.x0 + b.x1) as f64, (b.y0 + b.y1) as f64);
    let (cx, cy) = (sx / 2.0, sy / 2.0);
    let m: [f64; 6] = match kind {
        "rotate180" => [-1.0, 0.0, 0.0, -1.0, sx, sy],
        "rotate90Cw" => [0.0, 1.0, -1.0, 0.0, (cx + cy).round(), (cy - cx).round()],
        "rotate90Ccw" => [0.0, -1.0, 1.0, 0.0, (cx - cy).round(), (cx + cy).round()],
        "flipHorizontal" => [-1.0, 0.0, 0.0, 1.0, sx, 0.0],
        _ => [1.0, 0.0, 0.0, -1.0, 0.0, sy],
    };
    s.execute("edit.transform", json!({"layer": id.0, "matrix": m, "interpolation": "nearest"}))
}

// ---------- selection / clipboard ----------

fn reselect_target(s: &Session) -> Option<photocraft_raster::Surface> {
    let d = s.active()?;
    if d.doc.selection.is_some() {
        return None;
    }
    (0..d.history.past_len()).rev().find_map(|i| d.history.state(i).and_then(|doc| doc.selection.clone()))
}

fn can_reselect(s: &Session) -> std::result::Result<(), String> {
    has_doc(s)?;
    reselect_target(s).map(|_| ()).ok_or_else(|| "there is no selection to restore".into())
}

fn paste_into(s: &mut Session, p: &Value, outside: bool) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let selection = d.doc.selection.clone().ok_or(EngineError::Other("Paste Into needs a selection".into()))?;
    let b = selection.content_bounds();
    let key = step_key(s, "pasteInto");
    let mut params = json!({"center": [(b.x0 + b.x1) as f64 / 2.0, (b.y0 + b.y1) as f64 / 2.0], "coalesce": key});
    if let Some(c) = p.get("center") {
        params["center"] = c.clone();
    }
    let r = s.execute("edit.paste", params)?;
    let id = layer_param(s, &Value::Null)?;
    s.execute("layer.layerMask.revealAll", json!({"layer": id.0, "coalesce": key}))?;
    s.coalesce_request = Some(key);
    let r2 = s.edit(if outside { "Paste Outside" } else { "Paste Into" }, |doc, _| {
        let canvas = doc.bounds();
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let surface = if outside {
            // Everything but the selection: default reveal, the selection's area inverted.
            let area = b.intersect(&canvas);
            let inv: Vec<f32> = sel::mask_from_surface(Some(&selection), area).iter().map(|v| 1.0 - v).collect();
            let mut m = photocraft_raster::Surface::with_default(photocraft_color::PixelFormat::GRAY8, &[1.0]);
            m.write_region(area, &inv);
            m
        } else {
            selection
        };
        l.mask = Some(LayerMask { surface, linked: false, ..LayerMask::reveal_all() });
        Ok(())
    });
    s.coalesce_request = None;
    r2?;
    Ok(r)
}

// ---------- layers ----------

fn layer_from_background(s: &mut Session) -> Result<Value> {
    let id = s.edit("Layer From Background", |doc, active| {
        let l = doc.layers.first_mut().filter(|l| is_background(l)).ok_or(EngineError::Other("the document has no Background layer".into()))?;
        unlock_background(l);
        *active = Some(l.id);
        Ok(l.id)
    })?;
    Ok(json!({"layer": id.0}))
}

fn unlock_background(l: &mut Layer) {
    l.name = "Layer 0".into();
    l.locks.transparency = false;
    l.locks.position = false;
}

/// Before `id` gets a layer mask: the Background can't have one, so it becomes a normal layer
/// first ("Layer 0"), as when adding a mask to it in Photoshop. Other layers are left alone.
pub(crate) fn background_to_layer_for_mask(doc: &mut photocraft_doc::Document, id: photocraft_doc::LayerId) {
    if let Some(l) = doc.layers.first_mut().filter(|l| l.id == id && is_background(l)) {
        unlock_background(l);
    }
}

fn toggle_mask(s: &mut Session, p: &Value, key: &str) -> Result<Value> {
    let id = layer_param(s, p)?;
    let label = if key == "enabled" { "Enable/Disable Layer Mask" } else { "Link/Unlink Layer Mask" };
    let v = s.edit(label, |doc, _| {
        let m = doc.layer_mut(id).and_then(|l| l.mask.as_mut()).ok_or(EngineError::Other("the layer has no layer mask".into()))?;
        let slot = if key == "enabled" { &mut m.enabled } else { &mut m.linked };
        *slot = p.get(key).and_then(Value::as_bool).unwrap_or(!*slot);
        Ok(*slot)
    })?;
    Ok(json!({ key: v }))
}

fn set_all_effects(s: &mut Session, enabled: bool) -> Result<Value> {
    s.edit(if enabled { "Show All Effects" } else { "Hide All Effects" }, |doc, _| {
        fn rec(ls: &mut [Layer], on: bool) {
            for l in ls {
                if !l.effects.items.is_empty() {
                    l.effects.enabled = on;
                }
                if let Some(ch) = l.children_mut() {
                    rec(ch, on);
                }
            }
        }
        rec(&mut doc.layers, enabled);
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Remove every layer matching `pred` (whole subtrees), keeping at least one layer.
fn delete_where(s: &mut Session, label: &str, pred: fn(&Layer) -> bool) -> Result<Value> {
    let n = s.edit(label, |doc, active| {
        fn rec(ls: &mut Vec<Layer>, pred: fn(&Layer) -> bool) -> usize {
            let before = ls.len();
            ls.retain(|l| !pred(l));
            let mut n = before - ls.len();
            for l in ls.iter_mut() {
                if let Some(ch) = l.children_mut() {
                    n += rec(ch, pred);
                }
            }
            n
        }
        let backup = doc.layers.clone();
        let n = rec(&mut doc.layers, pred);
        if doc.layers.is_empty() {
            doc.layers = backup;
            return Err(EngineError::Other("a document must keep at least one layer".into()));
        }
        if active.is_none_or(|id| doc.layer(id).is_none()) {
            *active = doc.top_layer();
        }
        Ok(n)
    })?;
    Ok(json!({"deleted": n}))
}

fn is_empty_layer(l: &Layer) -> bool {
    match &l.content {
        LayerContent::Raster(s) => s.content_bounds().is_empty() && l.mask.is_none() && l.effects.items.is_empty(),
        LayerContent::Group(g) => g.children.iter().all(is_empty_layer),
        _ => false,
    }
}

fn ungroup(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_param(s, p)?;
    s.edit("Ungroup Layers", |doc, active| {
        let path = doc.path_of(id).ok_or(EngineError::NoLayer(id))?;
        let g = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        let LayerContent::Group(group) = &g.content else {
            return Err(EngineError::Other("the active layer is not a group".into()));
        };
        let children = group.children.clone();
        let (&idx, parent) = path.split_last().ok_or(EngineError::NoLayer(id))?;
        let sib = if parent.is_empty() { &mut doc.layers } else { doc.layer_at_mut(parent).and_then(Layer::children_mut).ok_or(EngineError::NoLayer(id))? };
        sib.remove(idx);
        let top = children.last().map(|l| l.id);
        for (k, l) in children.into_iter().enumerate() {
            sib.insert(idx + k, l);
        }
        *active = top.or_else(|| doc.top_layer());
        Ok(())
    })?;
    Ok(Value::Null)
}

fn set_visible(s: &mut Session, p: &Value, visible: bool) -> Result<Value> {
    if crate::layer_multi_cmds::multi(s, p) {
        return crate::layer_multi_cmds::set_visible_selected(s, visible);
    }
    let id = layer_param(s, p)?;
    s.edit(if visible { "Show Layer" } else { "Hide Layer" }, |doc, _| {
        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.visible = visible;
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Pixels of a fill or smart-object layer's content alone (no mask, effects or opacity).
fn content_pixels(doc: &Document, l: &Layer) -> photocraft_raster::Surface {
    let mut tmp = l.clone();
    tmp.mask = None;
    tmp.vector_mask = None;
    tmp.effects.items.clear();
    tmp.opacity = 1.0;
    tmp.fill_opacity = 1.0;
    tmp.blend = photocraft_color::BlendMode::Normal;
    tmp.clipped = false;
    tmp.visible = true;
    let canvas = doc.bounds();
    let buf = photocraft_compose::render_layer(&tmp, canvas);
    let fmt = doc.pixel_format();
    let n = fmt.channels();
    let mut data = vec![0.0f32; buf.px.len() * n];
    for (p, out) in buf.px.iter().zip(data.chunks_exact_mut(n)) {
        from_rgba_into(&fmt, *p, out);
    }
    let mut s = photocraft_raster::Surface::new(fmt);
    s.write_region(canvas, &data);
    s.prune();
    s
}

fn kind_of(s: &Session, id: LayerId) -> Option<&'static str> {
    let d = s.active()?;
    Some(match &d.doc.layer(id)?.content {
        LayerContent::Text(_) => "type",
        LayerContent::Shape(_) => "shape",
        LayerContent::Fill(_) => "fill",
        LayerContent::Smart(_) => "smart",
        _ => return None,
    })
}

/// Rasterize one layer; `only` restricts it to one kind. Returns false when there was nothing to do.
fn rasterize_one(s: &mut Session, id: LayerId, only: Option<&str>, key: &str) -> Result<bool> {
    let Some(kind) = kind_of(s, id).filter(|k| only.is_none_or(|o| o == *k)) else { return Ok(false) };
    match kind {
        "type" => {
            s.execute("type.rasterize", json!({"layer": id.0, "coalesce": key}))?;
        }
        "shape" => {
            s.execute("layer.rasterize.shape", json!({"layer": id.0, "coalesce": key}))?;
        }
        _ => {
            let label = if kind == "fill" { "Rasterize Fill Content" } else { "Rasterize Smart Object" };
            s.coalesce_request = Some(key.to_string());
            let r = s.edit(label, |doc, _| {
                let snap = doc.layer(id).cloned().ok_or(EngineError::NoLayer(id))?;
                let px = content_pixels(doc, &snap);
                let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                l.content = LayerContent::Raster(px);
                l.fill_cache = None;
                Ok(())
            });
            s.coalesce_request = None;
            r?;
        }
    }
    Ok(true)
}

fn rasterize(s: &mut Session, p: &Value, only: Option<&'static str>) -> Result<Value> {
    let id = layer_param(s, p)?;
    let key = step_key(s, "rasterize");
    if !rasterize_one(s, id, only, &key)? {
        return Err(EngineError::Other(match only {
            Some(k) => format!("the layer is not a {k} layer"),
            None => "the layer has nothing to rasterize".into(),
        }));
    }
    Ok(json!({"layer": id.0}))
}

fn rasterize_all(s: &mut Session) -> Result<Value> {
    let ids: Vec<LayerId> = s.active().map(|d| d.doc.walk().into_iter().map(|(_, _, l)| l.id).collect()).unwrap_or_default();
    let key = step_key(s, "rasterizeAll");
    let mut n = 0;
    for id in ids {
        // Every sub-step shares one history entry via the coalescing key.
        if rasterize_one(s, id, None, &key)? {
            n += 1;
        }
    }
    Ok(json!({"rasterized": n}))
}

// ---------- registry ----------

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $sc:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: $sc, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    vec![
        spec!(
            "edit.stroke",
            "Stroke…",
            &["Edit"],
            None,
            r##"{"width":1..250=1,"color":"#rrggbb|[r,g,b,a]"=foreground,"location":"inside|center|outside"="center","opacity":0..100=100}"##,
            has_pixels,
            stroke
        ),
        spec!("edit.transform.rotate180", "Rotate 180°", &["Edit", "Transform"], None, "{}", has_layer, |s, _| transform_preset(s, "rotate180")),
        spec!("edit.transform.rotate90Cw", "Rotate 90° Clockwise", &["Edit", "Transform"], None, "{}", has_layer, |s, _| transform_preset(s, "rotate90Cw")),
        spec!("edit.transform.rotate90Ccw", "Rotate 90° Counter Clockwise", &["Edit", "Transform"], None, "{}", has_layer, |s, _| transform_preset(
            s,
            "rotate90Ccw"
        )),
        spec!("edit.transform.flipHorizontal", "Flip Horizontal", &["Edit", "Transform"], None, "{}", has_layer, |s, _| transform_preset(s, "flipHorizontal")),
        spec!("edit.transform.flipVertical", "Flip Vertical", &["Edit", "Transform"], None, "{}", has_layer, |s, _| transform_preset(s, "flipVertical")),
        spec!(
            "edit.pasteSpecial.pasteInto",
            "Paste Into",
            &["Edit", "Paste Special"],
            Some("Cmd+Alt+Shift+V"),
            r##"{"center":[x,y]?}"##,
            has_clip_and_selection,
            |s, p| paste_into(s, p, false)
        ),
        spec!("edit.pasteSpecial.pasteOutside", "Paste Outside", &["Edit", "Paste Special"], None, r##"{"center":[x,y]?}"##, has_clip_and_selection, |s, p| {
            paste_into(s, p, true)
        }),
        spec!("select.reselect", "Reselect", &["Select"], Some("Cmd+Shift+D"), "{}", can_reselect, |s, _| {
            let m = reselect_target(s).ok_or(EngineError::Other("there is no selection to restore".into()))?;
            s.edit("Reselect", |doc, _| {
                doc.selection = Some(m);
                Ok(())
            })?;
            Ok(json!({"selected": true}))
        }),
        spec!("image.adjustments.equalize", "Equalize", &["Image", "Adjustments"], None, "{}", has_pixels, |s, _| equalize(s)),
        spec!("image.revealAll", "Reveal All", &["Image"], None, "{}", has_doc, |s, _| {
            let d = s.active().ok_or(EngineError::NoDocument)?;
            let canvas = d.doc.bounds();
            let all = d.doc.walk().into_iter().filter_map(|(_, _, l)| l.surface().map(|s| s.content_bounds())).fold(canvas, |a, b| a.union(&b));
            if all == canvas {
                return Ok(json!({"changed": false}));
            }
            s.execute("image.crop", json!({"x": all.x0, "y": all.y0, "width": all.width(), "height": all.height(), "deleteCroppedPixels": false}))?;
            Ok(json!({"changed": true}))
        }),
        spec!("layer.new.layerFromBackground", "Layer From Background…", &["Layer", "New"], None, "{}", has_background, |s, _| layer_from_background(s)),
        spec!("layer.layerStyle.copyLayerStyle", "Copy Layer Style", &["Layer", "Layer Style"], None, r##"{"layer":id?}"##, has_layer, |s, p| {
            let id = layer_param(s, p)?;
            let l = s.active().and_then(|d| d.doc.layer(id)).ok_or(EngineError::NoLayer(id))?;
            let style = (l.effects.clone(), l.blend, l.fill_opacity);
            s.style_clipboard = Some(style);
            Ok(Value::Null)
        }),
        spec!(
            "layer.layerStyle.pasteLayerStyle",
            "Paste Layer Style",
            &["Layer", "Layer Style"],
            None,
            r##"{"layer":id?}"##,
            |s| {
                has_layer(s)?;
                s.style_clipboard.as_ref().map(|_| ()).ok_or_else(|| "no layer style has been copied".into())
            },
            |s, p| {
                let id = layer_param(s, p)?;
                let (fx, blend, fill) = s.style_clipboard.clone().ok_or(EngineError::Other("no layer style has been copied".into()))?;
                s.edit("Paste Layer Style", |doc, _| {
                    let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                    l.effects = fx;
                    l.blend = blend;
                    l.fill_opacity = fill;
                    Ok(())
                })?;
                Ok(Value::Null)
            }
        ),
        spec!("layer.layerStyle.hideAllEffects", "Hide All Effects", &["Layer", "Layer Style"], None, "{}", has_doc, |s, _| set_all_effects(s, false)),
        spec!("layer.layerStyle.showAllEffects", "Show All Effects", &[], None, "{}", has_doc, |s, _| set_all_effects(s, true)),
        spec!(
            "layer.layerMask.enabled",
            "Disable Layer Mask",
            &["Layer", "Layer Mask"],
            None,
            r##"{"layer":id?,"enabled":bool? (default: toggle)}"##,
            has_mask,
            |s, p| toggle_mask(s, p, "enabled")
        ),
        spec!(
            "layer.layerMask.linked",
            "Unlink Layer Mask",
            &["Layer", "Layer Mask"],
            None,
            r##"{"layer":id?,"linked":bool? (default: toggle)}"##,
            has_mask,
            |s, p| toggle_mask(s, p, "linked")
        ),
        spec!("layer.rasterize.layer", "Layer", &["Layer", "Rasterize"], None, r##"{"layer":id?}"##, has_layer, |s, p| rasterize(s, p, None)),
        spec!("layer.rasterize.allLayers", "All Layers", &["Layer", "Rasterize"], None, "{}", has_doc, |s, _| rasterize_all(s)),
        spec!("layer.rasterize.type", "Type", &["Layer", "Rasterize"], None, r##"{"layer":id?}"##, has_layer, |s, p| rasterize(s, p, Some("type"))),
        spec!("layer.rasterize.fillContent", "Fill Content", &["Layer", "Rasterize"], None, r##"{"layer":id?}"##, has_layer, |s, p| rasterize(
            s,
            p,
            Some("fill")
        )),
        spec!("layer.rasterize.smartObject", "Smart Object", &["Layer", "Rasterize"], None, r##"{"layer":id?}"##, has_layer, |s, p| rasterize(
            s,
            p,
            Some("smart")
        )),
        spec!("layer.delete.hiddenLayers", "Hidden Layers", &["Layer", "Delete"], None, "{}", has_doc, |s, _| delete_where(s, "Delete Hidden Layers", |l| !l
            .visible)),
        spec!("file.scripts.deleteAllEmptyLayers", "Delete All Empty Layers", &["File", "Scripts"], None, "{}", has_doc, |s, _| delete_where(
            s,
            "Delete All Empty Layers",
            is_empty_layer
        )),
        spec!(
            "layer.ungroupLayers",
            "Ungroup Layers",
            &["Layer"],
            Some("Cmd+Shift+G"),
            r##"{"layer":id?}"##,
            |s| match active_layer(s)?.is_group() {
                true => Ok(()),
                false => Err("the active layer is not a group".into()),
            },
            ungroup
        ),
        spec!("layer.hideLayers", "Hide Layers", &["Layer"], Some("Cmd+,"), r##"{"layer":id?}"##, has_layer, |s, p| set_visible(s, p, false)),
        spec!("layer.showLayers", "Show Layers", &[], None, r##"{"layer":id?}"##, has_layer, |s, p| set_visible(s, p, true)),
        spec!("filter.blur.average", "Average", &["Filter", "Blur"], None, "{}", has_pixels, |s, _| average(s)),
        spec!("filter.render.clouds", "Clouds", &["Filter", "Render"], None, r##"{"seed":u32=0}"##, has_pixels, |s, p| clouds(s, p, false)),
        spec!("filter.render.differenceClouds", "Difference Clouds", &["Filter", "Render"], None, r##"{"seed":u32=0}"##, has_pixels, |s, p| clouds(s, p, true)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(depth: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 30, "depth": depth})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s
    }

    fn doc(s: &Session) -> &Document {
        &s.active().unwrap().doc
    }

    fn active(s: &Session) -> &Layer {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap()
    }

    fn pixel(s: &Session, x: i32, y: i32) -> Vec<f32> {
        active(s).surface().unwrap().pixel(x, y)
    }

    fn paint_square(s: &mut Session, r: Rect, c: [f32; 4]) {
        s.edit("paint", |doc, a| {
            doc.layer_mut(a.unwrap()).unwrap().surface_mut().unwrap().fill_rect(r, &c);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn stroke_locations_at_several_depths() {
        for depth in [8, 16, 32] {
            for (loc, inside, outside) in [("inside", 1.0, 0.0), ("outside", 0.0, 1.0), ("center", 1.0, 1.0)] {
                let mut s = session(depth);
                s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
                s.execute("edit.stroke", json!({"width": 2, "color": "#ff0000", "location": loc})).unwrap();
                assert!((pixel(&s, 10, 15)[3] - inside).abs() < 0.02, "{depth} {loc} inside {:?}", pixel(&s, 10, 15));
                assert!((pixel(&s, 9, 15)[3] - outside).abs() < 0.02, "{depth} {loc} outside {:?}", pixel(&s, 9, 15));
                assert_eq!(pixel(&s, 15, 15)[3], 0.0, "centre untouched");
            }
        }
        // Without a selection the layer's opaque edge is stroked.
        let mut s = session(8);
        paint_square(&mut s, Rect::new(10, 10, 20, 20), [0.0, 0.0, 1.0, 1.0]);
        s.execute("edit.stroke", json!({"width": 3, "color": "#00ff00", "location": "outside"})).unwrap();
        assert_eq!(pixel(&s, 8, 15), vec![0.0, 1.0, 0.0, 1.0]);
        assert_eq!(pixel(&s, 15, 15), vec![0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn transform_presets_are_exact() {
        let mut s = session(8);
        paint_square(&mut s, Rect::new(10, 10, 20, 14), [1.0, 0.0, 0.0, 1.0]);
        paint_square(&mut s, Rect::new(10, 10, 11, 11), [0.0, 1.0, 0.0, 1.0]);
        s.execute("edit.transform.flipHorizontal", json!({})).unwrap();
        assert_eq!(active(&s).surface().unwrap().content_bounds(), Rect::new(10, 10, 20, 14));
        assert_eq!(pixel(&s, 19, 10), vec![0.0, 1.0, 0.0, 1.0]);
        s.execute("edit.transform.flipVertical", json!({})).unwrap();
        assert_eq!(pixel(&s, 19, 13), vec![0.0, 1.0, 0.0, 1.0]);
        s.execute("edit.transform.rotate180", json!({})).unwrap();
        assert_eq!(pixel(&s, 10, 10), vec![0.0, 1.0, 0.0, 1.0]);
        s.execute("edit.transform.rotate90Cw", json!({})).unwrap();
        let b = active(&s).surface().unwrap().content_bounds();
        assert_eq!((b.width(), b.height()), (4, 10));
        // Top-left corner goes to the top-right after a clockwise turn.
        assert_eq!(pixel(&s, b.x1 - 1, b.y0), vec![0.0, 1.0, 0.0, 1.0]);
        s.execute("edit.transform.rotate90Ccw", json!({})).unwrap();
        assert_eq!(active(&s).surface().unwrap().content_bounds(), Rect::new(10, 10, 20, 14));
        assert_eq!(pixel(&s, 10, 10), vec![0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn reselect_restores_the_last_selection() {
        let mut s = session(8);
        assert!(!s.is_enabled("select.reselect"));
        s.execute("select.rect", json!({"x": 2, "y": 3, "width": 5, "height": 6})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        assert!(doc(&s).selection.is_none());
        s.execute("select.reselect", json!({})).unwrap();
        assert_eq!(doc(&s).selection.as_ref().unwrap().content_bounds(), Rect::new(2, 3, 7, 9));
        assert!(!s.is_enabled("select.reselect"), "nothing to reselect while a selection exists");
    }

    #[test]
    fn equalize_spreads_values() {
        for depth in [8, 16] {
            let mut s = session(depth);
            paint_square(&mut s, Rect::new(0, 0, 40, 15), [0.4, 0.4, 0.4, 1.0]);
            paint_square(&mut s, Rect::new(0, 15, 40, 30), [0.5, 0.5, 0.5, 1.0]);
            s.execute("image.adjustments.equalize", json!({})).unwrap();
            let (dark, light) = (pixel(&s, 1, 1)[0], pixel(&s, 1, 20)[0]);
            assert!(dark < 0.05 && light > 0.95, "{depth}: {dark} {light}");
        }
    }

    #[test]
    fn average_and_clouds() {
        let mut s = session(8);
        paint_square(&mut s, Rect::new(0, 0, 20, 30), [1.0, 0.0, 0.0, 1.0]);
        paint_square(&mut s, Rect::new(20, 0, 40, 30), [0.0, 0.0, 1.0, 1.0]);
        s.execute("filter.blur.average", json!({})).unwrap();
        let p = pixel(&s, 5, 5);
        assert!((p[0] - 0.5).abs() < 0.01 && (p[2] - 0.5).abs() < 0.01, "{p:?}");
        s.execute("filter.render.clouds", json!({"seed": 3})).unwrap();
        let vals: Vec<f32> = (0..40).map(|x| pixel(&s, x, 12)[0]).collect();
        let (lo, hi) = vals.iter().fold((1.0f32, 0.0f32), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(hi - lo > 0.05, "clouds vary: {lo}..{hi}");
        assert!(vals.windows(2).all(|w| (w[0] - w[1]).abs() < 0.2), "clouds are smooth");
        // Difference Clouds over itself with the same seed cancels to black.
        s.execute("filter.render.differenceClouds", json!({"seed": 3})).unwrap();
        assert!(pixel(&s, 7, 7)[..3].iter().all(|v| *v < 0.01), "{:?}", pixel(&s, 7, 7));
    }

    #[test]
    fn paste_into_masks_to_the_selection() {
        let mut s = session(8);
        paint_square(&mut s, Rect::new(0, 0, 10, 10), [1.0, 0.0, 0.0, 1.0]);
        s.execute("select.all", json!({})).unwrap();
        s.execute("edit.copy", json!({})).unwrap();
        let steps = s.active().unwrap().history.past_len();
        s.execute("select.rect", json!({"x": 20, "y": 10, "width": 8, "height": 8})).unwrap();
        s.execute("edit.pasteSpecial.pasteInto", json!({})).unwrap();
        let l = active(&s);
        let m = l.mask.as_ref().unwrap();
        assert!(!m.linked);
        assert_eq!(m.surface.content_bounds(), Rect::new(20, 10, 28, 18));
        assert_eq!(m.value(0, 0), 0.0);
        assert!(doc(&s).selection.is_none());
        assert_eq!(s.active().unwrap().history.past_len(), steps + 2, "select + one Paste Into step");
        // Paste Outside reveals everything but the selection.
        s.execute("select.rect", json!({"x": 20, "y": 10, "width": 8, "height": 8})).unwrap();
        s.execute("edit.pasteSpecial.pasteOutside", json!({})).unwrap();
        let m = active(&s).mask.as_ref().unwrap();
        assert_eq!((m.value(0, 0), m.value(22, 12)), (1.0, 0.0));
    }

    #[test]
    fn a_mask_turns_the_background_into_a_layer() {
        for (cmd, needs_selection) in [
            ("layer.layerMask.revealAll", false),
            ("layer.layerMask.hideAll", false),
            ("layer.layerMask.revealSelection", true),
            ("layer.layerMask.hideSelection", true),
        ] {
            let mut s = session(8);
            let bg = doc(&s).layers[0].id;
            if needs_selection {
                s.execute("select.rect", json!({"x": 2, "y": 2, "width": 10, "height": 10})).unwrap();
            }
            s.execute(cmd, json!({"layer": bg.0})).unwrap();
            let l = &doc(&s).layers[0];
            assert!(l.mask.is_some() && l.name == "Layer 0" && !l.locks.transparency && !l.locks.position, "{cmd}");
            assert!(!s.is_enabled("layer.new.layerFromBackground"), "{cmd}");
            // One step: undo brings the locked Background back, without a mask.
            assert!(s.undo());
            let l = &doc(&s).layers[0];
            assert!(l.mask.is_none() && l.name == "Background" && l.locks.transparency, "{cmd}");
        }
        // Other layers keep their name and locks.
        let mut s = session(8);
        let top = doc(&s).layers[1].id;
        s.execute("layer.setProps", json!({"layer": top.0, "locks": {"position": true}})).unwrap();
        s.execute("layer.layerMask.revealAll", json!({"layer": top.0})).unwrap();
        let l = doc(&s).layer(top).unwrap();
        assert!(l.mask.is_some() && l.name != "Layer 0" && l.locks.position);
        assert_eq!(doc(&s).layers[0].name, "Background");
    }

    #[test]
    fn layer_utilities() {
        let mut s = session(8);
        // Layer from Background.
        assert!(s.is_enabled("layer.new.layerFromBackground"));
        s.execute("layer.new.layerFromBackground", json!({})).unwrap();
        assert_eq!(doc(&s).layers[0].name, "Layer 0");
        assert!(!doc(&s).layers[0].locks.transparency);
        assert!(!s.is_enabled("layer.new.layerFromBackground"));
        // Copy / paste style and hide all effects.
        s.execute("layer.layerStyle.dropShadow", json!({})).unwrap();
        s.execute("layer.layerStyle.copyLayerStyle", json!({})).unwrap();
        let top = doc(&s).layers[1].id;
        s.execute("layer.layerStyle.pasteLayerStyle", json!({"layer": top.0})).unwrap();
        assert_eq!(doc(&s).layers[1].effects.items.len(), 1);
        s.execute("layer.layerStyle.hideAllEffects", json!({})).unwrap();
        assert!(doc(&s).walk().iter().all(|(_, _, l)| l.effects.items.is_empty() || !l.effects.enabled));
        // Mask toggles.
        s.execute("layer.layerMask.revealAll", json!({})).unwrap();
        s.execute("layer.layerMask.enabled", json!({})).unwrap();
        assert!(!active(&s).mask.as_ref().unwrap().enabled);
        s.execute("layer.layerMask.linked", json!({"linked": false})).unwrap();
        assert!(!active(&s).mask.as_ref().unwrap().linked);
        // Hide, delete hidden, delete empty.
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("layer.hideLayers", json!({})).unwrap();
        assert!(!active(&s).visible);
        assert_eq!(s.execute("layer.delete.hiddenLayers", json!({})).unwrap()["deleted"], 1);
        s.execute("layer.new.layer", json!({})).unwrap();
        let n = doc(&s).layer_count();
        assert_eq!(s.execute("file.scripts.deleteAllEmptyLayers", json!({})).unwrap()["deleted"], 1);
        assert_eq!(doc(&s).layer_count(), n - 1);
    }

    #[test]
    fn ungroup_restores_children_in_order() {
        let mut s = session(8);
        let a = active(&s).id;
        s.execute("layer.groupLayers", json!({})).unwrap();
        assert!(active(&s).is_group());
        s.execute("layer.ungroupLayers", json!({})).unwrap();
        assert_eq!(s.active().unwrap().active_layer, Some(a));
        assert!(doc(&s).layers.iter().all(|l| !l.is_group()));
        assert_eq!(doc(&s).layers.len(), 2);
    }

    #[test]
    fn reveal_all_grows_the_canvas() {
        let mut s = session(8);
        s.execute("layer.translate", json!({"dx": 0, "dy": 0})).ok();
        paint_square(&mut s, Rect::new(-5, 0, 10, 10), [1.0, 0.0, 0.0, 1.0]);
        s.execute("image.revealAll", json!({})).unwrap();
        assert_eq!((doc(&s).size.width, doc(&s).size.height), (45, 30));
        assert_eq!(s.execute("image.revealAll", json!({})).unwrap()["changed"], false);
    }

    #[test]
    fn rasterize_fill_and_all_in_one_step() {
        let mut s = session(8);
        s.execute("layer.newFillLayer.solidColor", json!({"color": "#00ff00"})).unwrap();
        s.execute("layer.newFillLayer.solidColor", json!({"color": "#0000ff"})).unwrap();
        s.execute("layer.rasterize.fillContent", json!({})).unwrap();
        assert!(matches!(active(&s).content, LayerContent::Raster(_)));
        assert_eq!(pixel(&s, 3, 3), vec![0.0, 0.0, 1.0, 1.0]);
        assert!(s.execute("layer.rasterize.type", json!({})).is_err());
        let steps = s.active().unwrap().history.past_len();
        assert_eq!(s.execute("layer.rasterize.allLayers", json!({})).unwrap()["rasterized"], 1);
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        assert!(doc(&s).walk().iter().all(|(_, _, l)| !matches!(l.content, LayerContent::Fill(_))));
    }
}
