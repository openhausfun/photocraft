//! Select menu and selection tools: Magic Wand, Color Range, Modify
//! (expand/contract/border/smooth/feather), Grow, Similar, Lasso.
//! Selections are grayscale coverage surfaces clipped to the canvas.

use photocraft_algo::selection::{self as sel, SelectionMode};
use photocraft_doc::Document;
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}
fn has_selection(s: &Session) -> std::result::Result<(), String> {
    s.active().filter(|d| d.doc.selection.is_some()).map(|_| ()).ok_or_else(|| "no selection".into())
}

fn f(p: &Value, k: &str, d: f32) -> f32 {
    p.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}
fn b(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn mode(p: &Value) -> SelectionMode {
    SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("replace"))
}

/// RGBA pixels the tools sample: the composite, or the active layer.
fn sample_pixels(s: &Session, all_layers: bool) -> Result<(Rect, Vec<[f32; 4]>)> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let area = d.doc.bounds();
    let layer = d.active_layer.and_then(|id| d.doc.layer(id)).and_then(|l| l.surface());
    match (all_layers, layer) {
        (false, Some(surf)) => {
            let mut px = vec![[0.0f32; 4]; area.width() as usize * area.height() as usize];
            surf.read_rgba_into(area, &mut px);
            Ok((area, px))
        }
        _ => Ok((area, photocraft_compose::render(&d.doc, area).px)),
    }
}

fn set_selection(s: &mut Session, label: &str, area: Rect, mask: Vec<f32>, m: SelectionMode) -> Result<Value> {
    let selected = s.edit(label, |doc, _| {
        doc.selection = sel::combine(doc.selection.as_ref(), &mask, area, m);
        Ok(doc.selection.is_some())
    })?;
    Ok(json!({ "selected": selected }))
}

fn current_mask(doc: &Document) -> (Rect, Vec<f32>) {
    let area = doc.bounds();
    (area, sel::mask_from_surface(doc.selection.as_ref(), area))
}

/// 8-bit RGBA of the active layer (or the composite with `all_layers`) over the canvas.
pub(crate) fn sample_rgba8(s: &Session, all_layers: bool) -> Result<(Rect, Vec<[u8; 4]>)> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let area = d.doc.bounds();
    let layer = d.active_layer.and_then(|id| d.doc.layer(id)).and_then(|l| l.surface());
    match (all_layers, layer) {
        (false, Some(surf)) => Ok((area, sel::rgba8_image(surf, area))),
        _ => {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            Ok((area, photocraft_compose::render(&d.doc, area).px.iter().map(|p| p.map(q)).collect()))
        }
    }
}

fn magic_wand(s: &mut Session, p: &Value) -> Result<Value> {
    let (x, y) = (f(p, "x", 0.0).floor() as i32, f(p, "y", 0.0).floor() as i32);
    let (area, img) = sample_rgba8(s, b(p, "sampleAllLayers", false))?;
    let region = sel::wand_region(&img, area, (x, y), f(p, "tolerance", 32.0), b(p, "contiguous", true), b(p, "antiAlias", true));
    drop(img);
    let m = mode(p);
    let selected = s.edit("Magic Wand", |doc, _| {
        doc.selection = sel::combine_region(doc.selection.as_ref(), region.as_ref(), m);
        Ok(doc.selection.is_some())
    })?;
    Ok(json!({ "selected": selected }))
}

fn parse_color(p: &Value) -> [f32; 3] {
    match p.get("color") {
        Some(Value::Array(a)) if a.len() >= 3 => [0, 1, 2].map(|i| a[i].as_f64().unwrap_or(0.0) as f32),
        Some(Value::String(h)) => {
            let h = h.trim_start_matches('#');
            let c = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).map_or(0.0, |v| f32::from(v) / 255.0);
            [c(0), c(2), c(4)]
        }
        _ => [0.0; 3],
    }
}

/// Eyedropper samples from `key` (`[[x, y], …]` in document pixels): the colour under each point
/// and where it was picked (area-local, for Localized Color Clusters).
fn eyedropper_samples(p: &Value, key: &str, area: Rect, px: &[[f32; 4]], bad: &impl Fn(String) -> EngineError) -> Result<Vec<sel::RangeSample>> {
    let Some(pts) = p.get(key) else { return Ok(Vec::new()) };
    let pts = pts.as_array().ok_or_else(|| bad(format!("`{key}` must be [[x, y], …]")))?;
    let w = area.width() as usize;
    let mut out = Vec::with_capacity(pts.len().min(4096));
    for pt in pts {
        let xy = pt.as_array().and_then(|a| match a.as_slice() {
            [x, y] => Some((x.as_f64()?, y.as_f64()?)),
            _ => None,
        });
        let Some((x, y)) = xy.filter(|(x, y)| x.is_finite() && y.is_finite()) else {
            return Err(bad(format!("bad point {pt} in `{key}` (want [x, y])")));
        };
        let (xi, yi) = (x.floor() as i32, y.floor() as i32);
        let q = if area.contains(xi, yi) { px.get((yi - area.y0) as usize * w + (xi - area.x0) as usize) } else { None };
        let Some(&[r, g, b, _]) = q else {
            return Err(bad(format!("point [{x}, {y}] in `{key}` is outside the canvas")));
        };
        out.push(sel::RangeSample { color: [r, g, b], at: Some(((xi - area.x0) as f32, (yi - area.y0) as f32)) });
    }
    Ok(out)
}

fn color_range(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "select.colorRange";
    let bad = |msg: String| EngineError::BadParams { cmd: CMD.into(), msg };
    let preset = p.get("select").map_or(Some("sampledColors"), Value::as_str).ok_or_else(|| bad("`select` must be a string".into()))?;
    let invert = b(p, "invert", false);
    let fuzz = |d: f32| f(p, "fuzziness", d).max(0.0);
    let (area, mut mask) = match preset {
        "sampledColors" => {
            let (area, px) = sample_pixels(s, b(p, "sampleAllLayers", true))?;
            let w = area.width() as usize;
            // The dialog's eyedroppers: colours picked on the image, at their positions.
            let mut samples = eyedropper_samples(p, "points", area, &px, &bad)?;
            let minus = eyedropper_samples(p, "subtractPoints", area, &px, &bad)?;
            if let Some(cs) = p.get("colors") {
                let cs = cs.as_array().ok_or_else(|| bad("`colors` must be a list of colours".into()))?;
                for c in cs {
                    samples.push(sel::RangeSample { color: parse_color(&json!({ "color": c })), at: None });
                }
            }
            if p.get("color").is_some() || samples.is_empty() {
                let color = if p.get("color").is_some() {
                    parse_color(p)
                } else {
                    let [r, g, b, _] = s.tools.foreground;
                    [r, g, b]
                };
                samples.push(sel::RangeSample { color, at: None });
            }
            // Localized Color Clusters: Range is a percentage of the canvas's longer side.
            let localized = if b(p, "localized", false) {
                if !samples.iter().chain(&minus).any(|x| x.at.is_some()) {
                    return Err(bad("localized clusters need eyedropper `points`".into()));
                }
                Some(f(p, "range", 100.0).clamp(0.0, 100.0) / 100.0 * area.width().max(area.height()) as f32)
            } else {
                None
            };
            let mut mask = sel::color_range_samples(&px, w, &samples, fuzz(40.0), localized);
            // The minus eyedropper: colours matching a subtracted sample (same Fuzziness) drop out.
            if !minus.is_empty() {
                let cut = sel::color_range_samples(&px, w, &minus, fuzz(40.0), localized);
                for (m, c) in mask.iter_mut().zip(&cut) {
                    *m *= 1.0 - c;
                }
            }
            (area, mask)
        }
        "reds" | "yellows" | "greens" | "cyans" | "blues" | "magentas" => {
            let center = match preset {
                "reds" => 0.0,
                "yellows" => 60.0,
                "greens" => 120.0,
                "cyans" => 180.0,
                "blues" => 240.0,
                _ => 300.0,
            };
            let (area, px) = sample_pixels(s, b(p, "sampleAllLayers", true))?;
            (area, sel::hue_range(&px, center))
        }
        "highlights" | "midtones" | "shadows" => {
            // Range in levels (Highlights / Shadows: one value, Midtones: [low, high]); Fuzziness
            // is a percentage of the tonal scale.
            let level = |v: &Value| v.as_f64().filter(|x| (0.0..=255.0).contains(x)).map(|x| x as f32);
            let r = p.get("tonalRange");
            let (lo, hi) = match (preset, r) {
                ("shadows", None) => (0.0, 65.0),
                ("highlights", None) => (190.0, 255.0),
                (_, None) => (105.0, 150.0),
                ("shadows", Some(v)) => (0.0, level(v).ok_or_else(|| bad(format!("tonalRange: want a level 0..255 (got {v})")))?),
                ("highlights", Some(v)) => (level(v).ok_or_else(|| bad(format!("tonalRange: want a level 0..255 (got {v})")))?, 255.0),
                (_, Some(v)) => v
                    .as_array()
                    .and_then(|a| match a.as_slice() {
                        [l, h] => Some((level(l)?, level(h)?)),
                        _ => None,
                    })
                    .filter(|(l, h)| l <= h)
                    .ok_or_else(|| bad(format!("tonalRange: want [low, high] levels 0..255 (got {v})")))?,
            };
            let (area, px) = sample_pixels(s, b(p, "sampleAllLayers", true))?;
            (area, sel::tone_range(&px, lo, hi, fuzz(20.0).min(100.0) / 100.0 * 255.0))
        }
        "outOfGamut" => {
            // Pixels the current proof setup (View › Proof Setup) can't reproduce, as the gamut
            // warning shows them: judged on the composite.
            let d = s.active().ok_or(EngineError::NoDocument)?;
            let pv = s.color.proof(d.doc.id);
            let (m, _) = crate::color_cmds::gamut_mask(&d.doc, &pv.setup, pv.gamut_threshold)?;
            (d.doc.bounds(), m.iter().map(|v| f32::from(*v) / 255.0).collect())
        }
        other => {
            return Err(bad(format!(
                "unknown select `{other}` (sampledColors, reds, yellows, greens, cyans, blues, magentas, highlights, midtones, shadows, outOfGamut)"
            )));
        }
    };
    if invert {
        for v in &mut mask {
            *v = 1.0 - *v;
        }
    }
    set_selection(s, "Color Range", area, mask, mode(p))
}

fn modify(s: &mut Session, p: &Value, op: &str) -> Result<Value> {
    // The dialogs' ranges (Photoshop's limits); anything else is refused, never clamped silently.
    let max = match op {
        "border" => 200.0,
        "feather" => 1000.0,
        _ => 500.0,
    };
    let r = f(p, "radius", 1.0);
    if !r.is_finite() || r > max {
        return Err(EngineError::BadParams { cmd: format!("select.modify.{op}"), msg: format!("radius must be a number in 0..{max}") });
    }
    let r = r.max(0.0);
    let (area, m) = current_mask(&s.active().ok_or(EngineError::NoDocument)?.doc);
    let (w, h) = (area.width() as usize, area.height() as usize);
    let out = match op {
        "expand" => sel::expand(&m, w, h, r),
        "contract" => sel::contract(&m, w, h, r),
        "border" => sel::border(&m, w, h, r),
        "smooth" => sel::smooth(&m, w, h, r),
        _ => sel::feather(&m, w, h, r),
    };
    let label = match op {
        "expand" => "Expand Selection",
        "contract" => "Contract Selection",
        "border" => "Border Selection",
        "smooth" => "Smooth Selection",
        _ => "Feather Selection",
    };
    set_selection(s, label, area, out, SelectionMode::Replace)
}

/// Grow (contiguous) / Similar (anywhere): pixels whose colour lies within
/// the selected pixels' colour range widened by the tolerance.
fn grow_similar(s: &mut Session, p: &Value, contiguous: bool) -> Result<Value> {
    let tol = f(p, "tolerance", 32.0) / 255.0;
    let (area, px) = sample_pixels(s, b(p, "sampleAllLayers", false))?;
    let (_, m) = current_mask(&s.active().ok_or(EngineError::NoDocument)?.doc);
    let (mut lo, mut hi) = ([f32::MAX; 4], [f32::MIN; 4]);
    for (q, v) in px.iter().zip(&m) {
        if *v >= 0.5 {
            for c in 0..4 {
                lo[c] = lo[c].min(q[c]);
                hi[c] = hi[c].max(q[c]);
            }
        }
    }
    if lo[0] > hi[0] {
        return Err(EngineError::Other("selection is empty".into()));
    }
    let inside = |q: &[f32; 4]| (0..4).all(|c| q[c] >= lo[c] - tol - 1e-6 && q[c] <= hi[c] + tol + 1e-6);
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut out: Vec<f32> = m.iter().map(|v| if *v >= 0.5 { 1.0 } else { 0.0 }).collect();
    if contiguous {
        let mut stack: Vec<usize> = (0..out.len()).filter(|i| out[*i] > 0.0).collect();
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            let n = [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)];
            for j in n.into_iter().flatten() {
                if out[j] == 0.0 && inside(&px[j]) {
                    out[j] = 1.0;
                    stack.push(j);
                }
            }
        }
    } else {
        for (o, q) in out.iter_mut().zip(&px) {
            if inside(q) {
                *o = 1.0;
            }
        }
    }
    set_selection(s, if contiguous { "Grow" } else { "Similar" }, area, out, SelectionMode::Replace)
}

fn lasso(s: &mut Session, p: &Value) -> Result<Value> {
    let pts: Vec<(f32, f32)> = p
        .get("points")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|q| Some((q.get(0)?.as_f64()? as f32, q.get(1)?.as_f64()? as f32))).collect())
        .unwrap_or_default();
    if pts.len() < 3 {
        return Err(EngineError::BadParams { cmd: "select.lasso".into(), msg: "needs at least 3 points".into() });
    }
    let area = s.active().ok_or(EngineError::NoDocument)?.doc.bounds();
    let mut mask = sel::polygon(&pts, area, b(p, "antiAlias", true));
    let feather = f(p, "feather", 0.0);
    if feather > 0.0 {
        mask = sel::feather(&mask, area.width() as usize, area.height() as usize, feather);
    }
    set_selection(s, "Lasso", area, mask, mode(p))
}

macro_rules! spec {
    ($id:literal, $label:literal, [$($m:literal),*], $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: None, params: $params, enabled: $en, run: $run, journal: true }
    };
}

/// Selection command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "select.magicWand",
            "Magic Wand",
            [],
            r##"{"x":px,"y":px,"tolerance":0..255=32,"contiguous":bool=true,"antiAlias":bool=true,"sampleAllLayers":bool=false,"mode":"replace|add|subtract|intersect"="replace"}"##,
            has_doc,
            magic_wand
        ),
        spec!(
            "select.colorRange",
            "Color Range…",
            ["Select"],
            r##"{"select":"sampledColors|reds|yellows|greens|cyans|blues|magentas|highlights|midtones|shadows|outOfGamut"="sampledColors","color":"#rrggbb"=foreground,"colors":["#rrggbb",…]?,"points":[[x,y],…]? (eyedropper samples),"subtractPoints":[[x,y],…]? (minus eyedropper),"fuzziness":0..200=40 (tones: 0..100 %=20),"localized":bool=false,"range":0..100=100 (% of the longer side),"tonalRange":level|[lo,hi] (shadows 65, highlights 190, midtones [105,150]),"invert":bool=false,"sampleAllLayers":bool=true,"mode":"replace|add|subtract|intersect"="replace"}"##,
            has_doc,
            color_range
        ),
        spec!("select.modify.border", "Border…", ["Select", "Modify"], r##"{"radius":1..200=1}"##, has_selection, |s, p| modify(s, p, "border")),
        spec!("select.modify.smooth", "Smooth…", ["Select", "Modify"], r##"{"radius":1..500=1}"##, has_selection, |s, p| modify(s, p, "smooth")),
        spec!("select.modify.expand", "Expand…", ["Select", "Modify"], r##"{"radius":1..500=1}"##, has_selection, |s, p| modify(s, p, "expand")),
        spec!("select.modify.contract", "Contract…", ["Select", "Modify"], r##"{"radius":1..500=1}"##, has_selection, |s, p| modify(s, p, "contract")),
        spec!("select.modify.feather", "Feather…", ["Select", "Modify"], r##"{"radius":0.1..1000=1}"##, has_selection, |s, p| modify(s, p, "feather")),
        spec!("select.grow", "Grow", ["Select"], r##"{"tolerance":0..255=32,"sampleAllLayers":bool=false}"##, has_selection, |s, p| grow_similar(s, p, true)),
        spec!("select.similar", "Similar", ["Select"], r##"{"tolerance":0..255=32,"sampleAllLayers":bool=false}"##, has_selection, |s, p| grow_similar(
            s, p, false
        )),
        spec!(
            "select.lasso",
            "Lasso",
            [],
            r##"{"points":[[x,y],…],"mode":"replace|add|subtract|intersect"="replace","antiAlias":bool=true,"feather":px=0}"##,
            has_doc,
            lasso
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 30})).unwrap();
        s.edit("paint", |doc, _| {
            let bg = doc.layers[0].surface_mut().unwrap();
            bg.fill_rect(Rect::new(5, 5, 15, 15), &[1.0, 0.0, 0.0, 1.0]);
            bg.fill_rect(Rect::new(25, 5, 35, 15), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
        s
    }

    fn coverage(s: &Session, x: i32, y: i32) -> f32 {
        s.active().unwrap().doc.selection.as_ref().map_or(0.0, |m| m.sample_channel(x, y, 0))
    }

    #[test]
    fn magic_wand_contiguous_and_modes() {
        let mut s = session();
        s.execute("select.magicWand", json!({"x": 7, "y": 7, "antiAlias": false})).unwrap();
        assert_eq!(coverage(&s, 10, 10), 1.0);
        assert_eq!(coverage(&s, 30, 10), 0.0);
        s.execute("select.magicWand", json!({"x": 30, "y": 7, "mode": "add", "antiAlias": false})).unwrap();
        assert_eq!(coverage(&s, 30, 10), 1.0);
        s.execute("select.magicWand", json!({"x": 7, "y": 7, "mode": "subtract", "antiAlias": false})).unwrap();
        assert_eq!(coverage(&s, 10, 10), 0.0);
        s.execute("select.magicWand", json!({"x": 7, "y": 7, "contiguous": false, "antiAlias": false})).unwrap();
        assert_eq!(coverage(&s, 30, 10), 1.0);
    }

    #[test]
    fn color_range_selects_by_colour() {
        let mut s = session();
        s.execute("select.colorRange", json!({"color": "#ff0000", "fuzziness": 10})).unwrap();
        assert_eq!(coverage(&s, 10, 10), 1.0);
        assert_eq!(coverage(&s, 20, 20), 0.0);
    }

    /// Red squares (from `session`), a blue square, a black and a mid-grey strip on white.
    fn session_colours() -> Session {
        let mut s = session();
        s.edit("paint", |doc, _| {
            let bg = doc.layers[0].surface_mut().unwrap();
            bg.fill_rect(Rect::new(5, 20, 15, 28), &[0.0, 0.0, 1.0, 1.0]);
            bg.fill_rect(Rect::new(20, 20, 25, 28), &[0.0, 0.0, 0.0, 1.0]);
            bg.fill_rect(Rect::new(30, 20, 35, 28), &[0.5, 0.5, 0.5, 1.0]);
            Ok(())
        })
        .unwrap();
        s
    }

    #[test]
    fn color_range_several_samples_points_and_invert() {
        let mut s = session_colours();
        s.execute("select.colorRange", json!({"colors": ["#ff0000", "#0000ff"], "fuzziness": 10})).unwrap();
        assert_eq!((coverage(&s, 10, 10), coverage(&s, 30, 10), coverage(&s, 10, 24)), (1.0, 1.0, 1.0));
        assert_eq!(coverage(&s, 2, 2), 0.0);
        // Eyedropper point on the blue square; `invert` flips the result.
        s.execute("select.colorRange", json!({"points": [[7, 22]], "fuzziness": 10, "invert": true})).unwrap();
        assert_eq!((coverage(&s, 10, 24), coverage(&s, 10, 10), coverage(&s, 2, 2)), (0.0, 1.0, 1.0));
        // Localized clusters: picked on the left red square, a 25% range (10 px) leaves the right one.
        s.execute("select.colorRange", json!({"points": [[10, 10]], "fuzziness": 10, "localized": true, "range": 25})).unwrap();
        assert_eq!(coverage(&s, 10, 10), 1.0);
        assert!(coverage(&s, 13, 10) > 0.5);
        assert_eq!(coverage(&s, 30, 10), 0.0);
        // One undo step per call.
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(coverage(&s, 2, 2), 1.0);
    }

    #[test]
    fn color_range_presets() {
        let mut s = session_colours();
        s.execute("select.colorRange", json!({"select": "reds"})).unwrap();
        assert_eq!((coverage(&s, 10, 10), coverage(&s, 30, 10)), (1.0, 1.0));
        assert_eq!((coverage(&s, 10, 24), coverage(&s, 2, 2), coverage(&s, 32, 24)), (0.0, 0.0, 0.0));
        s.execute("select.colorRange", json!({"select": "blues"})).unwrap();
        assert_eq!((coverage(&s, 10, 24), coverage(&s, 10, 10)), (1.0, 0.0));
        s.execute("select.colorRange", json!({"select": "greens"})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_none() || coverage(&s, 10, 10) == 0.0);
        // Tones: black is a shadow, white a highlight, 50% grey (128) a midtone.
        s.execute("select.colorRange", json!({"select": "shadows"})).unwrap();
        assert_eq!((coverage(&s, 22, 24), coverage(&s, 2, 2), coverage(&s, 32, 24)), (1.0, 0.0, 0.0));
        s.execute("select.colorRange", json!({"select": "highlights"})).unwrap();
        assert_eq!((coverage(&s, 22, 24), coverage(&s, 2, 2)), (0.0, 1.0));
        s.execute("select.colorRange", json!({"select": "midtones", "fuzziness": 0})).unwrap();
        assert_eq!((coverage(&s, 32, 24), coverage(&s, 2, 2), coverage(&s, 22, 24)), (1.0, 0.0, 0.0));
        // A wider shadow range takes the grey too.
        s.execute("select.colorRange", json!({"select": "shadows", "tonalRange": 140, "fuzziness": 0})).unwrap();
        assert_eq!(coverage(&s, 32, 24), 1.0);
        // Modes combine as for the other selection commands.
        s.execute("select.colorRange", json!({"select": "reds", "mode": "add"})).unwrap();
        assert_eq!((coverage(&s, 32, 24), coverage(&s, 10, 10)), (1.0, 1.0));
    }

    #[test]
    fn color_range_out_of_gamut_matches_the_gamut_warning() {
        let mut s = session();
        s.edit("paint", |doc, _| {
            // Saturated sRGB blue is outside the default CMYK proof; mid grey is inside.
            let bg = doc.layers[0].surface_mut().unwrap();
            bg.fill_rect(Rect::new(0, 20, 20, 30), &[0.0, 0.0, 1.0, 1.0]);
            bg.fill_rect(Rect::new(20, 20, 40, 30), &[0.5, 0.5, 0.5, 1.0]);
            Ok(())
        })
        .unwrap();
        s.execute("select.colorRange", json!({"select": "outOfGamut"})).unwrap();
        assert_eq!((coverage(&s, 5, 25), coverage(&s, 30, 25)), (1.0, 0.0));
        let warn = s.execute("view.gamutWarning", json!({"on": true})).unwrap();
        let sel = s.active().unwrap().doc.selection.clone().unwrap();
        let mut n = 0;
        for y in 0..30 {
            for x in 0..40 {
                n += usize::from(sel.sample_channel(x, y, 0) > 0.5);
            }
        }
        assert_eq!(warn["outOfGamut"].as_u64(), Some(n as u64));
    }

    #[test]
    fn color_range_rejects_bad_params() {
        let mut s = session_colours();
        for p in [
            json!({"select": "skin"}),
            json!({"select": 3}),
            json!({"points": [[100, 2]]}),
            json!({"points": [[1]]}),
            json!({"points": "here"}),
            json!({"colors": "#ff0000"}),
            json!({"localized": true}),
            json!({"select": "midtones", "tonalRange": [200, 100]}),
            json!({"select": "shadows", "tonalRange": 300}),
        ] {
            assert!(s.execute("select.colorRange", p.clone()).is_err(), "{p}");
        }
        assert!(s.active().unwrap().doc.selection.is_none());
    }

    #[test]
    fn modify_commands() {
        let mut s = session();
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
        s.execute("select.modify.expand", json!({"radius": 2})).unwrap();
        assert_eq!(coverage(&s, 8, 15), 1.0);
        s.execute("select.modify.contract", json!({"radius": 4})).unwrap();
        assert_eq!(coverage(&s, 11, 15), 0.0);
        assert_eq!(coverage(&s, 15, 15), 1.0);
        s.execute("select.modify.border", json!({"radius": 2})).unwrap();
        assert_eq!(coverage(&s, 15, 15), 0.0);
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
        s.execute("select.modify.feather", json!({"radius": 4})).unwrap();
        let e = coverage(&s, 10, 15);
        assert!(e > 0.2 && e < 0.8, "{e}");
        s.execute("select.modify.smooth", json!({"radius": 2})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_some());
        assert!(!s.is_enabled("select.nothing"));
        // Out-of-range or non-finite radii are refused and leave the selection alone.
        for op in ["feather", "smooth", "expand", "contract", "border"] {
            for r in [json!(1e300), json!(5000), json!(f64::MAX)] {
                assert!(s.execute(&format!("select.modify.{op}"), json!({"radius": r})).is_err(), "{op} {r}");
            }
        }
        assert!(s.active().unwrap().doc.selection.is_some());
    }

    #[test]
    fn grow_and_similar() {
        let mut s = session();
        s.execute("select.rect", json!({"x": 6, "y": 6, "width": 2, "height": 2})).unwrap();
        s.execute("select.grow", json!({"tolerance": 5})).unwrap();
        assert_eq!(coverage(&s, 14, 14), 1.0);
        assert_eq!(coverage(&s, 30, 10), 0.0);
        s.execute("select.similar", json!({"tolerance": 5})).unwrap();
        assert_eq!(coverage(&s, 30, 10), 1.0);
        assert_eq!(coverage(&s, 20, 20), 0.0);
    }

    #[test]
    fn lasso_polygon() {
        let mut s = session();
        s.execute("select.lasso", json!({"points": [[0, 0], [20, 0], [0, 20]]})).unwrap();
        assert_eq!(coverage(&s, 2, 2), 1.0);
        assert_eq!(coverage(&s, 18, 18), 0.0);
        assert!(s.execute("select.lasso", json!({"points": [[0, 0], [1, 1]]})).is_err());
        s.execute("select.lasso", json!({"points": [[0, 0], [20, 0], [0, 20]], "mode": "subtract"})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_none());
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(coverage(&s, 2, 2), 1.0);
    }
}
