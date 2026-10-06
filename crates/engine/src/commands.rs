//! The command registry. Ids follow Photoshop's menu structure (see the parity checklist).

use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
use photocraft_doc::{Adjustment, Document, Fill, Layer, LayerContent, LayerId, LayerMask, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::{EngineError, Result, Session, inspect, pixels};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// Metadata + implementation for one command.
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Menu placement, e.g. `["Layer", "New"]`. Empty = not in menus.
    pub menu: &'static [&'static str],
    /// Default shortcut in egui-style notation (`Cmd+Shift+N`), mapped per platform by the UI.
    pub shortcut: Option<&'static str>,
    /// Human/agent-readable parameter description (JSON-ish).
    pub params: &'static str,
    pub enabled: Enabled,
    pub run: Run,
    /// Record in the session journal (false for queries).
    pub journal: bool,
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}
fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}
fn has_layer(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.filter(|id| d.doc.layer(*id).is_some()).map(|_| ()).ok_or_else(|| "no active layer".into())
}
fn has_pixel_layer(s: &Session) -> std::result::Result<(), String> {
    let l = crate::active_layer_of(s)?;
    if matches!(l.content, LayerContent::Raster(_)) { Ok(()) } else { Err(format!("active layer is a {} layer, not a pixel layer", l.content.kind_name())) }
}
/// A pixel layer, or a targeted alpha channel / Quick Mask (adjustments and fills apply to it).
fn has_pixel_or_channel(s: &Session) -> std::result::Result<(), String> {
    if crate::channel_cmds::edits_channel(s) {
        return Ok(());
    }
    has_pixel_layer(s)
}
/// A layer that can be painted: pixels, or any layer with a mask (paint with `"target":"mask"`).
pub(crate) fn has_paintable(s: &Session) -> std::result::Result<(), String> {
    if crate::channel_cmds::edits_channel(s) {
        return Ok(());
    }
    let l = crate::active_layer_of(s)?;
    if matches!(l.content, LayerContent::Raster(_)) || l.mask.is_some() {
        Ok(())
    } else {
        Err(format!("active layer is a {} layer without a mask", l.content.kind_name()))
    }
}

/// Surface a paint command writes to: the layer's pixels, or its mask with `"target":"mask"`.
pub(crate) fn paint_surface<'a>(l: &'a mut Layer, p: &Value) -> Result<&'a mut photocraft_raster::Surface> {
    if !is_mask_target(p) && (l.locks.pixels || l.locks.all) {
        return Err(EngineError::Other(format!("Could not complete your request because the layer \"{}\" is locked", l.name)));
    }
    if is_mask_target(p) {
        l.mask.as_mut().map(|m| &mut m.surface).ok_or_else(|| EngineError::Other("layer has no mask".into()))
    } else {
        l.surface_mut().ok_or_else(|| EngineError::Other("not a pixel layer".into()))
    }
}

pub(crate) fn is_mask_target(p: &Value) -> bool {
    p.get("target").and_then(Value::as_str) == Some("mask")
}

fn can_undo(s: &Session) -> std::result::Result<(), String> {
    s.active().filter(|d| d.history.can_undo()).map(|_| ()).ok_or_else(|| "nothing to undo".into())
}
fn can_redo(s: &Session) -> std::result::Result<(), String> {
    s.active().filter(|d| d.history.can_redo()).map(|_| ()).ok_or_else(|| "nothing to redo".into())
}
fn has_selection(s: &Session) -> std::result::Result<(), String> {
    s.active().filter(|d| d.doc.selection.is_some()).map(|_| ()).ok_or_else(|| "no selection".into())
}

/// Image › Image Rotation by a right angle or a flip (pixels, vectors, guides, … all move).
fn turn(s: &mut Session, label: &str, t: crate::canvas_geom::Turn) -> Result<Value> {
    s.edit(label, |doc, _| {
        crate::canvas_geom::turn_canvas(doc, t);
        Ok(())
    })?;
    Ok(Value::Null)
}

macro_rules! cmd {
    ($id:literal, $label:literal, [$($m:literal),*], $sc:expr, $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: $sc, params: $params, enabled: $en, run: $run, journal: true }
    };
}

pub fn command_specs() -> &'static [CommandSpec] {
    static SPECS: std::sync::OnceLock<Vec<CommandSpec>> = std::sync::OnceLock::new();
    SPECS.get_or_init(build)
}

pub fn find(id: &str) -> Option<&'static CommandSpec> {
    command_specs().iter().find(|c| c.id == id)
}

// ---------- param helpers ----------

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
/// Integer parameter that also accepts JSON floats (UIs send `12.0`); rounds.
pub(crate) fn int(p: &Value, key: &str) -> Option<i64> {
    p.get(key).and_then(|v| v.as_i64().or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f.round() as i64)))
}

fn f32_or(p: &Value, key: &str, default: f32) -> f32 {
    p.get(key).and_then(Value::as_f64).map(|v| v as f32).unwrap_or(default)
}
pub(crate) fn color_param(p: &Value, key: &str, default: [f32; 4]) -> [f32; 4] {
    match p.get(key) {
        Some(Value::Array(a)) if a.len() >= 3 => {
            let v: Vec<f32> = a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
            [v[0], v[1], v[2], v.get(3).copied().unwrap_or(1.0)]
        }
        Some(Value::String(s)) => parse_hex(s).unwrap_or(default),
        _ => default,
    }
}
fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let s = s.trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok().map(|v| v as f32 / 255.0);
    match s.len() {
        6 => Some([b(0)?, b(2)?, b(4)?, 1.0]),
        8 => Some([b(0)?, b(2)?, b(4)?, b(6)?]),
        _ => None,
    }
}
pub(crate) fn layer_param(s: &Session, p: &Value) -> Result<LayerId> {
    match p.get("layer").and_then(Value::as_u64) {
        Some(id) => Ok(LayerId(id)),
        None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into())),
    }
}
pub fn blend_from_str(s: &str) -> Option<BlendMode> {
    let norm = |x: &str| x.to_ascii_lowercase().replace([' ', '_', '-', '(', ')'], "");
    let want = norm(s);
    std::iter::once(BlendMode::PassThrough).chain(BlendMode::LAYER_MODES).find(|m| norm(m.label()) == want || norm(&format!("{m:?}")) == want)
}

fn new_adjustment(s: &mut Session, adj: Adjustment) -> Result<Value> {
    let label = format!("New {} Layer", adj.label());
    let name = adj.label().to_string();
    let id = s.edit(&label, |doc, active| {
        let l = Layer::new(doc.next_layer_name(&name), LayerContent::Adjustment(adj));
        let id = doc.insert_above(*active, l);
        *active = Some(id);
        Ok(id)
    })?;
    Ok(json!({ "layer": id.0 }))
}

fn destructive_adjust(s: &mut Session, label: &str, adj: Adjustment, p: &Value) -> Result<Value> {
    if crate::channel_cmds::is_channel_target(p) {
        // Alpha channel / Quick Mask target: the adjustment runs on the grayscale channel.
        return s.edit(label, |doc, _| {
            let sel = doc.selection.clone();
            if let Some(surf) = crate::channel_cmds::channel_surface_for_filter(doc, p)? {
                pixels::adjust_surface(surf, &adj, sel.as_ref(), ColorMode::Grayscale);
                surf.prune();
            }
            Ok(Value::Null)
        });
    }
    let id = layer_param(s, &Value::Null)?;
    s.edit(label, |doc, _| {
        let sel = doc.selection.clone();
        let mode = doc.mode;
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let surf = l.surface_mut().ok_or_else(|| EngineError::Other("not a pixel layer".into()))?;
        pixels::adjust_surface(surf, &adj, sel.as_ref(), mode);
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Lenient adjustment from params for previews: bad params fall back to the kind's defaults.
/// Commands use the checked [`crate::adjust_params::from_params`].
pub fn adjustment_from_params(kind: &str, p: &Value) -> Adjustment {
    crate::adjust_params::from_params(kind, p, None, ColorMode::Rgb)
        .or_else(|_| crate::adjust_params::default_for(kind, ColorMode::Rgb))
        .unwrap_or(Adjustment::Invert)
}

fn doc_mode(s: &Session) -> ColorMode {
    s.active().map_or(ColorMode::Rgb, |d| d.doc.mode)
}

// ---------- the table ----------

#[allow(clippy::too_many_lines)]
fn build() -> Vec<CommandSpec> {
    let mut v = vec![
        // File
        cmd!(
            "file.new",
            "New…",
            ["File"],
            Some("Cmd+N"),
            r##"{"width":u32=1920,"height":u32=1080,"mode":"rgb|gray|cmyk|lab"="rgb","depth":8|16|32=8,"background":"white|black|backgroundColor|transparent|#rrggbb"="white","resolution":ppi=72,"name":str}"##,
            always,
            |s, p| {
                // A size given as a float (`512.0`, as JSON from a UI field) is still that size (#254).
                let px = |k: &str, d: u32| match p.get(k) {
                    Some(v) => v
                        .as_u64()
                        .map(|n| n.clamp(1, 300_000) as u32)
                        .or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f.round().clamp(1.0, 300_000.0) as u32))
                        .unwrap_or(d),
                    None => d,
                };
                let (w, h) = (px("width", 1920), px("height", 1080));
                let mode = match p.get("mode").and_then(Value::as_str).unwrap_or("rgb") {
                    "gray" | "grayscale" => ColorMode::Grayscale,
                    "cmyk" => ColorMode::Cmyk,
                    "lab" => ColorMode::Lab,
                    _ => ColorMode::Rgb,
                };
                let depth = match p.get("depth").and_then(Value::as_u64).unwrap_or(8) {
                    16 => SampleType::U16,
                    32 => SampleType::F32,
                    _ => SampleType::U8,
                };
                let name = p.get("name").and_then(Value::as_str).unwrap_or("Untitled").to_string();
                let res = p.get("resolution").and_then(Value::as_f64).unwrap_or(72.0).clamp(1.0, 30_000.0) as f32;
                let bgc = s.tools.background;
                let mut doc = match p.get("background").and_then(Value::as_str).unwrap_or("white") {
                    "backgroundColor" => Document::with_background(name, Size::new(w, h), mode, depth, Color::rgba(bgc[0], bgc[1], bgc[2], 1.0)),
                    "transparent" => {
                        let mut d = Document::new(name, Size::new(w, h), mode, depth);
                        d.layers.push(Layer::raster("Layer 1", d.pixel_format()));
                        d
                    }
                    "black" => Document::with_background(name, Size::new(w, h), mode, depth, Color::BLACK),
                    other => {
                        let c = parse_hex(other).unwrap_or([1.0; 4]);
                        Document::with_background(name, Size::new(w, h), mode, depth, Color::rgba(c[0], c[1], c[2], c[3]))
                    }
                };
                doc.resolution_dpi = res;
                let i = s.add_document(doc, None);
                Ok(json!({ "document": i }))
            }
        ),
        cmd!("file.close", "Close", ["File"], Some("Cmd+W"), r##"{"document":index?}"##, has_doc, |s, p| {
            let i = p.get("document").and_then(Value::as_u64).map(|v| v as usize).or(s.active_index()).ok_or(EngineError::NoDocument)?;
            s.close(i).ok_or(EngineError::NoDocument)?;
            Ok(Value::Null)
        }),
        // Edit
        cmd!("edit.undo", "Undo", ["Edit"], Some("Cmd+Z"), "{}", can_undo, |s, _| Ok(json!(s.undo()))),
        cmd!("edit.redo", "Redo", ["Edit"], Some("Cmd+Shift+Z"), "{}", can_redo, |s, _| Ok(json!(s.redo()))),
        cmd!(
            "edit.fill",
            "Fill…",
            ["Edit"],
            Some("Shift+F5"),
            r##"{"contents":"foreground|background|color|contentAware|pattern|history|black|gray|white"="color","color":"#rrggbb|[r,g,b,a]"=foreground (contents=color),"pattern":id|name (contents=pattern),"scale":%=100,"angle":deg,"state":index? (contents=history; default the oldest state),"colorAdaptation":bool=true (contents=contentAware),"mode":"normal|multiply|…"="normal","opacity":0..100=100,"preserveTransparency":bool=false,"target":"pixels"|{"channel":i}|"quickMask"?}"##,
            has_pixel_or_channel,
            crate::fill_cmds::fill
        ),
        cmd!("edit.clear", "Clear", ["Edit"], Some("Delete"), "{}", has_pixel_layer, |s, p| {
            let id = layer_param(s, p)?;
            let bg = s.tools.background;
            s.edit("Clear", |doc, _| {
                let sel = doc.selection.clone();
                let area = sel.as_ref().map(|m| m.content_bounds()).unwrap_or(doc.bounds());
                crate::edit_cmds::clear_area(doc, id, area, sel.as_ref(), bg)
            })?;
            Ok(Value::Null)
        }),
        // Select
        cmd!("select.all", "All", ["Select"], Some("Cmd+A"), "{}", has_doc, |s, _| {
            s.edit("Select All", |doc, _| {
                let mut m = Surface::new(photocraft_color::PixelFormat::GRAY8);
                m.fill_rect(doc.bounds(), &[1.0]);
                doc.selection = Some(m);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("select.deselect", "Deselect", ["Select"], Some("Cmd+D"), "{}", has_selection, |s, _| {
            s.edit("Deselect", |doc, _| {
                doc.selection = None;
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("select.inverse", "Inverse", ["Select"], Some("Cmd+Shift+I"), "{}", has_selection, |s, _| {
            s.edit("Inverse", |doc, _| {
                let old = doc.selection.take().unwrap_or_else(|| Surface::new(photocraft_color::PixelFormat::GRAY8));
                let b = doc.bounds();
                let data: Vec<f32> = old.read_region(b).into_iter().map(|v| 1.0 - v).collect();
                let mut m = Surface::new(photocraft_color::PixelFormat::GRAY8);
                m.write_region(b, &data);
                m.prune();
                doc.selection = Some(m);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "select.rect",
            "Rectangular Selection",
            [],
            None,
            r##"{"x":i32,"y":i32,"width":u32,"height":u32,"mode":"replace|add|subtract|intersect"="replace","ellipse":bool=false,"antiAlias":bool=true,"feather":px=0}"##,
            has_doc,
            |s, p| {
                let get = |k: &str| int(p, k).ok_or_else(|| bad("select.rect", format!("missing `{k}`")));
                let r = Rect::from_xywh(get("x")? as i32, get("y")? as i32, get("width")?.max(0) as u32, get("height")?.max(0) as u32);
                let mode = p.get("mode").and_then(Value::as_str).unwrap_or("replace").to_string();
                let ellipse = p.get("ellipse").and_then(Value::as_bool).unwrap_or(false);
                // Options bar: anti-aliased ellipse edges (4x4 supersampled) and Feather (applied to the new shape only).
                let aa = p.get("antiAlias").and_then(Value::as_bool).unwrap_or(true);
                let feather = p.get("feather").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1000.0) as f32;
                s.edit("Rectangular Marquee", |doc, _| {
                    let area = doc.bounds();
                    // A marquee dragged past the canvas stops at its edge (Photoshop); the ellipse
                    // keeps the dragged shape and is only cut there.
                    let cut = r.intersect(&area);
                    let mut shape = Surface::new(photocraft_color::PixelFormat::GRAY8);
                    if ellipse {
                        let (cx, cy) = ((r.x0 + r.x1) as f32 / 2.0, (r.y0 + r.y1) as f32 / 2.0);
                        let (rx, ry) = (r.width() as f32 / 2.0, r.height() as f32 / 2.0);
                        let inside = |x: f32, y: f32| {
                            let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
                            dx * dx + dy * dy <= 1.0
                        };
                        for y in cut.y0..cut.y1 {
                            for x in cut.x0..cut.x1 {
                                let (fx, fy) = (x as f32, y as f32);
                                let corners = [(fx, fy), (fx + 1.0, fy), (fx, fy + 1.0), (fx + 1.0, fy + 1.0)].iter().filter(|(a, b)| inside(*a, *b)).count();
                                let cov = if !aa {
                                    if inside(fx + 0.5, fy + 0.5) { 1.0 } else { 0.0 }
                                } else if corners == 4 {
                                    1.0
                                } else {
                                    let n = (0..16).filter(|i| inside(fx + ((i % 4) as f32 + 0.5) / 4.0, fy + ((i / 4) as f32 + 0.5) / 4.0)).count();
                                    n as f32 / 16.0
                                };
                                if cov > 0.0 {
                                    shape.write_pixel(x, y, &[cov]);
                                }
                            }
                        }
                    } else {
                        shape.fill_rect(cut, &[1.0]);
                    }
                    if feather > 0.0 {
                        use photocraft_algo::selection as sel;
                        let m = sel::feather(&sel::mask_from_surface(Some(&shape), area), area.width() as usize, area.height() as usize, feather);
                        doc.selection = sel::combine(doc.selection.as_ref(), &m, area, sel::SelectionMode::parse(&mode));
                        return Ok(());
                    }
                    let old = doc.selection.take();
                    let combined = match (mode.as_str(), old) {
                        ("add", Some(o)) => combine(&o, &shape, area, |a, b| a.max(b)),
                        ("subtract", Some(o)) => combine(&o, &shape, area, |a, b| a * (1.0 - b)),
                        ("intersect", Some(o)) => combine(&o, &shape, area, |a, b| a.min(b)),
                        _ => shape,
                    };
                    // Nothing selected on the canvas (e.g. dragged entirely outside it) deselects.
                    doc.selection = (!combined.content_bounds().is_empty()).then_some(combined);
                    Ok(())
                })?;
                Ok(Value::Null)
            }
        ),
        // Layer
        cmd!("layer.new.layer", "Layer…", ["Layer", "New"], Some("Cmd+Shift+N"), r##"{"name":str?}"##, has_doc, |s, p| {
            let id = s.edit("New Layer", |doc, active| {
                let name = p.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| doc.next_layer_name("Layer"));
                let id = doc.insert_above(*active, Layer::raster(name, doc.pixel_format()));
                *active = Some(id);
                Ok(id)
            })?;
            Ok(json!({ "layer": id.0 }))
        }),
        cmd!("layer.new.group", "Group…", ["Layer", "New"], None, r##"{"name":str?}"##, has_doc, |s, p| {
            let id = s.edit("New Group", |doc, active| {
                let name = p.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| doc.next_layer_name("Group"));
                let id = doc.insert_above(*active, Layer::group(name, vec![]));
                *active = Some(id);
                Ok(id)
            })?;
            Ok(json!({ "layer": id.0 }))
        }),
        cmd!(
            "layer.groupLayers",
            "Group Layers",
            ["Layer"],
            Some("Cmd+G"),
            r##"{"layer":id?,"name":str?} (no layer: every selected layer)"##,
            has_layer,
            crate::layer_multi_cmds::group_layers
        ),
        cmd!("layer.duplicate", "Duplicate Layer…", ["Layer"], None, r##"{"layer":id?} (no layer: every selected layer)"##, has_layer, |s, p| {
            if crate::layer_multi_cmds::multi(s, p) {
                return crate::layer_multi_cmds::duplicate_selected(s);
            }
            let id = layer_param(s, p)?;
            let nid = s.edit("Duplicate Layer", |doc, active| {
                let src = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
                // A copy of the Background layer is an ordinary, unlocked layer (Photoshop).
                let from_background = src.name == "Background" && src.locks.transparency && doc.layers.first().is_some_and(|b| b.id == id);
                let mut dup = src.duplicate();
                dup.name = format!("{} copy", dup.name);
                if from_background {
                    dup.locks = Default::default();
                }
                let nid = doc.insert_above(Some(id), dup);
                *active = Some(nid);
                Ok(nid)
            })?;
            Ok(json!({ "layer": nid.0 }))
        }),
        cmd!("layer.delete", "Delete Layer", ["Layer", "Delete"], None, r##"{"layer":id?} (no layer: every selected layer)"##, has_layer, |s, p| {
            if crate::layer_multi_cmds::multi(s, p) {
                return crate::layer_multi_cmds::delete_selected(s);
            }
            let id = layer_param(s, p)?;
            s.edit("Delete Layer", |doc, active| {
                doc.remove(id).ok_or(EngineError::NoLayer(id))?;
                if *active == Some(id) {
                    *active = doc.top_layer();
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "layer.select",
            "Select Layer",
            [],
            None,
            r##"{"layer":id,"mode":"replace|toggle|range|add"="replace"} (toggle = ⌘-click, range = ⇧-click)"##,
            has_doc,
            crate::layer_multi_cmds::select
        ),
        cmd!(
            "layer.setProps",
            "Layer Properties",
            [],
            None,
            r##"{"layer":id?,"name":str?,"visible":bool?,"opacity":0..1?,"fill":0..1?,"blend":"Multiply|…"?,"clipped":bool?,"locked":bool?,"locks":{"transparency","pixels","position","artboard","all":bool}?,"channels":[bool,…]? (Advanced Blending: which colour channels blend, R G B / C M Y K / L a b)}"##,
            has_layer,
            |s, p| {
                let id = layer_param(s, p)?;
                let label = if p.get("visible").is_some() && p.as_object().is_some_and(|o| o.len() <= 2) { "Layer Visibility" } else { "Layer Properties" };
                let before = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
                // Clipping and channel changes reach the layers around it: recomposite everything.
                let local = p.get("clipped").is_none() && p.get("channels").is_none();
                s.edit(label, |doc, _| {
                    let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                    if let Some(v) = p.get("name").and_then(Value::as_str) {
                        l.name = v.to_string();
                    }
                    if let Some(v) = p.get("visible").and_then(Value::as_bool) {
                        l.visible = v;
                    }
                    if let Some(v) = p.get("opacity").and_then(Value::as_f64) {
                        l.opacity = (v as f32).clamp(0.0, 1.0);
                    }
                    if let Some(v) = p.get("fill").and_then(Value::as_f64) {
                        l.fill_opacity = (v as f32).clamp(0.0, 1.0);
                    }
                    if let Some(v) = p.get("blend").and_then(Value::as_str) {
                        l.blend = blend_from_str(v).ok_or_else(|| bad("layer.setProps", format!("unknown blend mode `{v}`")))?;
                    }
                    if let Some(v) = p.get("clipped").and_then(Value::as_bool) {
                        l.clipped = v;
                    }
                    if let Some(v) = p.get("locked").and_then(Value::as_bool) {
                        l.locks.all = v;
                    }
                    if let Some(Value::Array(ch)) = p.get("channels") {
                        // Blending Options › Advanced Blending › Channels: unchecked = left out.
                        l.excluded_channels = ch.iter().take(32).enumerate().filter(|(_, v)| v.as_bool() == Some(false)).fold(0, |m, (i, _)| m | 1 << i);
                    }
                    if let Some(Value::Object(m)) = p.get("locks") {
                        for (k, v) in m {
                            let v = v.as_bool().unwrap_or(false);
                            match k.as_str() {
                                "transparency" => l.locks.transparency = v,
                                "pixels" => l.locks.pixels = v,
                                "position" => l.locks.position = v,
                                "artboard" => l.locks.artboard = v,
                                "all" => l.locks.all = v,
                                other => return Err(bad("layer.setProps", format!("unknown lock `{other}`"))),
                            }
                        }
                    }
                    Ok(())
                })?;
                if local {
                    crate::layer_multi_cmds::note_damage(s, &before, &[id]);
                }
                Ok(Value::Null)
            }
        ),
        cmd!("layer.arrange.bringForward", "Bring Forward", ["Layer", "Arrange"], Some("Cmd+]"), r##"{"layer":id?}"##, has_layer, |s, p| arrange(s, p, 1)),
        cmd!("layer.arrange.sendBackward", "Send Backward", ["Layer", "Arrange"], Some("Cmd+["), r##"{"layer":id?}"##, has_layer, |s, p| arrange(s, p, -1)),
        cmd!("layer.arrange.bringToFront", "Bring to Front", ["Layer", "Arrange"], Some("Cmd+Shift+]"), r##"{"layer":id?}"##, has_layer, |s, p| arrange(
            s,
            p,
            i32::MAX
        )),
        cmd!("layer.arrange.sendToBack", "Send to Back", ["Layer", "Arrange"], Some("Cmd+Shift+["), r##"{"layer":id?}"##, has_layer, |s, p| arrange(
            s,
            p,
            i32::MIN
        )),
        cmd!("layer.createClippingMask", "Create Clipping Mask", ["Layer"], Some("Cmd+Alt+G"), r##"{"layer":id?}"##, has_layer, |s, p| {
            let id = layer_param(s, p)?;
            s.edit("Create Clipping Mask", |doc, _| {
                doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.clipped = true;
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("layer.releaseClippingMask", "Release Clipping Mask", ["Layer"], None, r##"{"layer":id?}"##, has_layer, |s, p| {
            let id = layer_param(s, p)?;
            s.edit("Release Clipping Mask", |doc, _| {
                doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.clipped = false;
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("layer.layerMask.revealAll", "Reveal All", ["Layer", "Layer Mask"], None, r##"{"layer":id?}"##, has_layer, |s, p| set_mask(
            s,
            p,
            "Add Layer Mask",
            Some(LayerMask::reveal_all())
        )),
        cmd!("layer.layerMask.hideAll", "Hide All", ["Layer", "Layer Mask"], None, r##"{"layer":id?}"##, has_layer, |s, p| set_mask(
            s,
            p,
            "Add Layer Mask",
            Some(LayerMask::hide_all())
        )),
        cmd!("layer.layerMask.revealSelection", "Reveal Selection", ["Layer", "Layer Mask"], None, r##"{"layer":id?}"##, has_selection, |s, p| {
            let sel = s.active().and_then(|d| d.doc.selection.clone()).ok_or(EngineError::Other("no selection".into()))?;
            let mask = LayerMask { surface: sel, ..LayerMask::reveal_all() };
            set_mask(s, p, "Add Layer Mask", Some(mask))
        }),
        cmd!("layer.layerMask.delete", "Delete", ["Layer", "Layer Mask"], None, r##"{"layer":id?}"##, has_layer, |s, p| set_mask(
            s,
            p,
            "Delete Layer Mask",
            None
        )),
        cmd!("layer.mergeDown", "Merge Down", ["Layer"], None, r##"{"layer":id?}"##, has_layer, |s, p| {
            let id = layer_param(s, p)?;
            s.edit("Merge Down", |doc, active| {
                let path = doc.path_of(id).ok_or(EngineError::NoLayer(id))?;
                let Some((&idx, parent)) = path.split_last() else { return Err(EngineError::NoLayer(id)) };
                if idx == 0 {
                    return Err(EngineError::Other("no layer below to merge into".into()));
                }
                let mut below = parent.to_vec();
                below.push(idx - 1);
                let lower = doc.layer_at(&below).ok_or(EngineError::NoLayer(id))?.clone();
                let upper = doc.layer(id).ok_or(EngineError::NoLayer(id))?.clone();
                let merged = pixels::merge_down(doc.bounds(), &lower, &upper, doc.pixel_format());
                doc.remove(id);
                *doc.layer_at_mut(&below).ok_or(EngineError::NoLayer(lower.id))? = merged;
                *active = Some(lower.id);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("layer.flattenImage", "Flatten Image", ["Layer"], None, "{}", has_doc, |s, _| {
            s.edit("Flatten Image", |doc, active| {
                let fmt = doc.pixel_format();
                let mut bg = Layer::raster("Background", fmt);
                bg.locks.transparency = true;
                *crate::pixels_mut(&mut bg)? = photocraft_compose::flatten_to_surface(doc, fmt, Some([1.0, 1.0, 1.0]));
                *active = Some(bg.id);
                doc.layers = vec![bg];
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("layer.newFillLayer.solidColor", "Solid Color…", ["Layer", "New Fill Layer"], None, r##"{"color":"#rrggbb"=foreground}"##, has_doc, |s, p| {
            let c = color_param(p, "color", s.tools.foreground);
            let id = s.edit("New Color Fill Layer", |doc, active| {
                let l = Layer::new(doc.next_layer_name("Color Fill"), LayerContent::Fill(Fill::Solid(Color::rgba(c[0], c[1], c[2], c[3]))));
                let id = doc.insert_above(*active, l);
                *active = Some(id);
                Ok(id)
            })?;
            Ok(json!({ "layer": id.0 }))
        }),
        cmd!(
            "layer.newFillLayer.gradient",
            "Gradient…",
            ["Layer", "New Fill Layer"],
            None,
            r##"{"from":"#rrggbb","to":"#rrggbb","angle":deg=90,"style":"linear|radial|angle|reflected|diamond","reverse":bool}"##,
            has_doc,
            |s, p| {
                let a = color_param(p, "from", s.tools.foreground);
                let b = color_param(p, "to", s.tools.background);
                let angle = f32_or(p, "angle", 90.0);
                let style = crate::layer_style::gradient_style(p.get("style").and_then(Value::as_str).unwrap_or("linear"));
                let reverse = p.get("reverse").and_then(Value::as_bool).unwrap_or(false);
                let id = s.edit("New Gradient Fill Layer", |doc, active| {
                    let fill = Fill::gradient(
                        vec![(0.0, Color::rgba(a[0], a[1], a[2], a[3])), (1.0, Color::rgba(b[0], b[1], b[2], b[3]))],
                        angle,
                        1.0,
                        style,
                        reverse,
                    );
                    let id = doc.insert_above(*active, Layer::new(doc.next_layer_name("Gradient Fill"), LayerContent::Fill(fill)));
                    *active = Some(id);
                    Ok(id)
                })?;
                Ok(json!({ "layer": id.0 }))
            }
        ),
        cmd!("layer.setAdjustment", "Adjustment Properties", [], None, r##"{"layer":id?, …params of that adjustment kind}"##, has_layer, |s, p| {
            let id = layer_param(s, p)?;
            s.edit("Modify Adjustment", |doc, _| {
                let mode = doc.mode;
                let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                let LayerContent::Adjustment(adj) = &mut l.content else { return Err(EngineError::Other("not an adjustment layer".into())) };
                if matches!(adj, Adjustment::Unsupported { .. }) {
                    return Err(EngineError::Other("this adjustment can't be edited (kept as imported)".into()));
                }
                let kind = adjustment_kind(adj);
                // No params resets to the defaults; otherwise params merge over the current values.
                *adj = if crate::adjust_params::user_keys(p).is_empty() {
                    crate::adjust_params::default_for(kind, mode)?
                } else {
                    crate::adjust_params::from_params(kind, p, Some(adj), mode)?
                };
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("layer.moveTo", "Reorder Layer", [], None, r##"{"layer":id?,"target":id,"position":"above|below|into"="above"}"##, has_layer, |s, p| {
            let id = layer_param(s, p)?;
            let target = LayerId(p.get("target").and_then(Value::as_u64).ok_or_else(|| bad("layer.moveTo", "missing `target`"))?);
            let pos = p.get("position").and_then(Value::as_str).unwrap_or("above").to_string();
            if id == target {
                return Ok(Value::Null);
            }
            s.edit("Reorder Layers", |doc, active| {
                // Refuse to move a group into itself or its descendants.
                if doc.layer(id).is_some_and(|l| contains_layer(l, target)) {
                    return Err(EngineError::Other("can't move a group into itself".into()));
                }
                let layer = doc.remove(id).ok_or(EngineError::NoLayer(id))?;
                let path = doc.path_of(target).ok_or(EngineError::NoLayer(target))?;
                match pos.as_str() {
                    "into" => {
                        let g = doc.layer_at_mut(&path).and_then(|t| t.children_mut()).ok_or_else(|| EngineError::Other("target is not a group".into()))?;
                        g.push(layer);
                    }
                    other => {
                        let (&last, parent) = path.split_last().ok_or(EngineError::NoLayer(target))?;
                        let sib = if parent.is_empty() {
                            &mut doc.layers
                        } else {
                            doc.layer_at_mut(parent)
                                .and_then(|t| t.children_mut())
                                .ok_or_else(|| EngineError::Other("target's parent is not a group".into()))?
                        };
                        let at = if other == "below" { last } else { last + 1 };
                        sib.insert(at.min(sib.len()), layer);
                    }
                }
                *active = Some(id);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "layer.translate",
            "Move Layer",
            [],
            None,
            r##"{"layer":id?,"dx":i32,"dy":i32} (no layer: every selected layer; linked layers follow)"##,
            has_layer,
            crate::layer_multi_cmds::translate
        ),
        // Image
        cmd!("image.imageRotation.flipCanvasHorizontal", "Flip Canvas Horizontal", ["Image", "Image Rotation"], None, "{}", has_doc, |s, _| {
            turn(s, "Flip Canvas Horizontal", crate::canvas_geom::Turn::FlipHorizontal)
        }),
        cmd!("image.imageRotation.flipCanvasVertical", "Flip Canvas Vertical", ["Image", "Image Rotation"], None, "{}", has_doc, |s, _| {
            turn(s, "Flip Canvas Vertical", crate::canvas_geom::Turn::FlipVertical)
        }),
        cmd!("image.imageRotation.180", "180°", ["Image", "Image Rotation"], None, "{}", has_doc, |s, _| {
            turn(s, "Rotate Canvas", crate::canvas_geom::Turn::Rotate180)
        }),
        cmd!("image.imageRotation.90cw", "90° Clockwise", ["Image", "Image Rotation"], None, "{}", has_doc, |s, _| {
            turn(s, "Rotate Canvas", crate::canvas_geom::Turn::Cw90)
        }),
        cmd!("image.imageRotation.90ccw", "90° Counter Clockwise", ["Image", "Image Rotation"], None, "{}", has_doc, |s, _| {
            turn(s, "Rotate Canvas", crate::canvas_geom::Turn::Ccw90)
        }),
        cmd!("image.adjustments.desaturate", "Desaturate", ["Image", "Adjustments"], Some("Cmd+Shift+U"), "{}", has_pixel_layer, |s, _| {
            destructive_adjust(
                s,
                "Desaturate",
                Adjustment::HueSaturation {
                    hue: 0.0,
                    saturation: -100.0,
                    lightness: 0.0,
                    colorize: false,
                    ranges: photocraft_doc::adjust::HueRange::defaults(),
                },
                &Value::Null,
            )
        }),
        // Paint
        cmd!(
            "paint.stroke",
            "Brush Stroke",
            [],
            None,
            r##"{"points":[[x,y,pressure?,tiltX?,tiltY?,rotation?,timeMs?,wheel?],…],"brush":{…BrushSettings}?,"preset":name?,"size":px?,"hardness":0..1?,"opacity":0..1?,"flow":0..1?,"spacing":0..10?,"color":"#rrggbb"?=foreground,"mode":"normal|multiply|screen|…"="normal","erase":bool?,"smoothing":0..1?,"zoom":number=1,"seed":u64?,"target":"pixels"|"mask"|"quickMask"|{"channel":i}=Channels panel target}"##,
            has_paintable,
            crate::brush_cmds::paint_stroke
        ),
        cmd!("tools.setColors", "Set Colors", [], None, r##"{"foreground":"#rrggbb"?,"background":"#rrggbb"?}"##, always, |s, p| {
            s.tools.foreground = color_param(p, "foreground", s.tools.foreground);
            s.tools.background = color_param(p, "background", s.tools.background);
            Ok(Value::Null)
        }),
        cmd!("tools.swapColors", "Switch Foreground and Background Colors", [], Some("X"), "{}", always, |s, _| {
            std::mem::swap(&mut s.tools.foreground, &mut s.tools.background);
            Ok(Value::Null)
        }),
        cmd!("tools.defaultColors", "Default Foreground and Background Colors", [], Some("D"), "{}", always, |s, _| {
            s.tools.foreground = [0.0, 0.0, 0.0, 1.0];
            s.tools.background = [1.0, 1.0, 1.0, 1.0];
            Ok(Value::Null)
        }),
        // Queries (not journaled)
        CommandSpec {
            id: "session.inspect",
            label: "Inspect Session",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: |s, _| Ok(inspect::session(s)),
            journal: false,
        },
        CommandSpec {
            id: "document.inspect",
            label: "Inspect Document",
            menu: &[],
            shortcut: None,
            params: r##"{"document":index?}"##,
            enabled: has_doc,
            run: |s, p| {
                let i = p.get("document").and_then(Value::as_u64).map(|v| v as usize).or(s.active_index()).ok_or(EngineError::NoDocument)?;
                let d = s.documents().get(i).ok_or(EngineError::NoDocument)?;
                Ok(inspect::document(d))
            },
            journal: false,
        },
        CommandSpec {
            id: "document.activate",
            label: "Activate Document",
            menu: &[],
            shortcut: None,
            params: r##"{"document":index}"##,
            enabled: has_doc,
            run: |s, p| {
                let i = p.get("document").and_then(Value::as_u64).ok_or_else(|| bad("document.activate", "missing `document`"))?;
                if s.set_active(i as usize) { Ok(Value::Null) } else { Err(EngineError::NoDocument) }
            },
            journal: false,
        },
        CommandSpec {
            id: "command.list",
            label: "List Commands",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: |s, _| {
                Ok(Value::Array(
                    command_specs()
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id, "label": c.label, "menu": c.menu, "shortcut": c.shortcut, "params": c.params, "enabled": (c.enabled)(s).is_ok(),
                            })
                        })
                        .collect(),
                ))
            },
            journal: false,
        },
        CommandSpec {
            id: "document.pixel",
            label: "Read Composite Pixel",
            menu: &[],
            shortcut: None,
            params: r##"{"x":i32,"y":i32}"##,
            enabled: has_doc,
            run: |s, p| {
                let x = int(p, "x").unwrap_or(0) as i32;
                let y = int(p, "y").unwrap_or(0) as i32;
                let d = s.active().ok_or(EngineError::NoDocument)?;
                let px = photocraft_compose::render(&d.doc, Rect::from_xywh(x, y, 1, 1)).px[0];
                Ok(json!(px))
            },
            journal: false,
        },
    ];

    // Adjustment layers + destructive adjustments, generated from one list.
    // Levels/Curves channel keys follow the document: red/green/blue (RGB), gray (Grayscale),
    // cyan/magenta/yellow/black (CMYK), lightness/a/b (Lab); see `adjust_params`.
    const ADJ: &[(&str, &str, &str)] = &[
        ("brightnessContrast", "Brightness/Contrast…", r##"{"brightness":-150..150=0,"contrast":-50..100=0,"legacy":bool=false}"##),
        (
            "levels",
            "Levels…",
            r##"{"inBlack":0..253=0,"gamma":0.01..9.99=1,"inWhite":2..255=255,"outBlack":0..255=0,"outWhite":0..255=255,"red":json,"green":json,"blue":json} (top level = composite; per channel {"inBlack","gamma","inWhite","outBlack","outWhite"} under red/green/blue, gray, cyan/magenta/yellow/black or lightness/a/b)"##,
        ),
        (
            "curves",
            "Curves…",
            r##"{"points":json,"red":json,"green":json,"blue":json} (curves as [[in,out],…] in 0..255, 2..19 points: points = composite; red/green/blue, gray, cyan/magenta/yellow/black or lightness/a/b per channel)"##,
        ),
        ("exposure", "Exposure…", r##"{"exposure":-20..20=0,"offset":-0.5..0.5=0,"gamma":0.01..9.99=1}"##),
        ("vibrance", "Vibrance…", r##"{"vibrance":-100..100=0,"saturation":-100..100=0}"##),
        (
            "hueSaturation",
            "Hue/Saturation…",
            r##"{"hue":-180..180=0,"saturation":-100..100=0,"lightness":-100..100=0,"colorize":bool=false,"reds":json,"yellows":json,"greens":json,"cyans":json,"blues":json,"magentas":json} (per range {"hue","saturation","lightness","range":[4 hue degrees]}; colorize: hue 0..360, saturation 0..100)"##,
        ),
        (
            "colorBalance",
            "Color Balance…",
            r##"{"shadows":json,"midtones":json,"highlights":json,"preserveLuminosity":bool=true} (each tone [cyan-red, magenta-green, yellow-blue] in -100..100)"##,
        ),
        (
            "blackWhite",
            "Black & White…",
            r##"{"reds":-200..300=40,"yellows":-200..300=60,"greens":-200..300=40,"cyans":-200..300=60,"blues":-200..300=20,"magentas":-200..300=80,"tint":bool=false,"tintColor":"#rrggbb"}"##,
        ),
        (
            "photoFilter",
            "Photo Filter…",
            r##"{"filter":"warming85|warmingLBA|warming81|cooling80|coolingLBB|cooling82|red|orange|yellow|green|cyan|blue|violet|magenta|sepia|deepRed|deepBlue|deepEmerald|deepYellow|underwater","color":"#rrggbb","density":0..100=25,"preserveLuminosity":bool=true}"##,
        ),
        (
            "channelMixer",
            "Channel Mixer…",
            r##"{"red":json,"green":json,"blue":json,"gray":json,"monochrome":bool=false} (per output channel [red %, green %, blue %, constant %] in -200..200; gray = the monochrome mix)"##,
        ),
        ("invert", "Invert", "{}"),
        ("posterize", "Posterize…", r##"{"levels":2..255=4}"##),
        ("threshold", "Threshold…", r##"{"level":1..255=128}"##),
        ("gradientMap", "Gradient Map…", r##"{"stops":json,"reverse":bool=false,"dither":bool=false} (stops [[location 0..1, "#rrggbb"], …], 2..64)"##),
        (
            "selectiveColor",
            "Selective Color…",
            r##"{"method":"relative|absolute"="relative","colors":"reds|yellows|greens|cyans|blues|magentas|whites|neutrals|blacks"="reds","cyan":-100..100=0,"magenta":-100..100=0,"yellow":-100..100=0,"black":-100..100=0,"reds":json} (per-range [c,m,y,k] arrays: reds yellows greens cyans blues magentas whites neutrals blacks)"##,
        ),
        (
            "colorLookup",
            "Color Lookup…",
            r##"{"lut":"none|warm|cool|tealOrange|bleachBypass|fadedFilm|dayForNight|monoContrast|crossProcess"="none","file":text,"interpolation":"trilinear|tetrahedral"="trilinear","dither":bool=false,"data":json} (file: .cube/.3dl/.look path; data: file text + "fileName")"##,
        ),
    ];
    for &(kind, label, params) in ADJ {
        let layer_id: &'static str = Box::leak(format!("layer.newAdjustmentLayer.{kind}").into_boxed_str());
        let image_id: &'static str = Box::leak(format!("image.adjustments.{kind}").into_boxed_str());
        v.push(CommandSpec {
            id: layer_id,
            label,
            menu: &["Layer", "New Adjustment Layer"],
            shortcut: None,
            params,
            enabled: has_doc,
            run: |s, p| {
                let kind = p.get("__kind").and_then(Value::as_str).unwrap_or("invert").to_string();
                let adj = crate::adjust_params::from_params(&kind, p, None, doc_mode(s))?;
                new_adjustment(s, adj)
            },
            journal: true,
        });
        v.push(CommandSpec {
            id: image_id,
            label,
            menu: &["Image", "Adjustments"],
            shortcut: match kind {
                "levels" => Some("Cmd+L"),
                "curves" => Some("Cmd+M"),
                "hueSaturation" => Some("Cmd+U"),
                "colorBalance" => Some("Cmd+B"),
                "invert" => Some("Cmd+I"),
                "blackWhite" => Some("Cmd+Alt+Shift+B"),
                _ => None,
            },
            params,
            enabled: has_pixel_or_channel,
            run: |s, p| {
                let kind = p.get("__kind").and_then(Value::as_str).unwrap_or("invert").to_string();
                let adj = crate::adjust_params::from_params(&kind, p, None, doc_mode(s))?;
                let label = adj.label().to_string();
                destructive_adjust(s, &label, adj, p)
            },
            journal: true,
        });
    }
    v.extend(crate::layer_style::specs());
    v.extend(crate::filters::specs());
    v.extend(crate::filters_ext::specs());
    v.extend(crate::gallery_cmds::specs());
    v.extend(crate::gradient_fill_cmds::specs());
    v.extend(crate::type_cmds::specs());
    v.extend(crate::transform_cmds::specs());
    v.extend(crate::vector_cmds::specs());
    v.extend(crate::smartselect_cmds::specs());
    v.extend(crate::edit_cmds::specs());
    v.extend(crate::color_cmds::specs());
    v.extend(crate::brush_cmds::specs());
    v.extend(crate::brush_preset_cmds::specs());
    v.extend(crate::eraser_cmds::specs());
    v.extend(crate::preset_import_cmds::specs());
    v.extend(crate::retouch_cmds::specs());
    v.extend(crate::image_cmds::specs());
    v.extend(crate::selection_cmds::specs());
    v.extend(crate::select_extra_cmds::specs());
    v.extend(crate::paint_cmds::specs());
    v.extend(crate::extra_cmds::specs());
    v.extend(crate::file_cmds::specs());
    v.extend(crate::type_extra_cmds::specs());
    v.extend(crate::type_styles_cmds::specs());
    v.extend(crate::type_spell_cmds::specs());
    v.extend(crate::smart_cmds::specs());
    v.extend(crate::layer_multi_cmds::specs());
    v.extend(crate::prefs::specs());
    v.extend(crate::edit_menu_cmds::specs());
    v.extend(crate::fill_key_cmds::specs());
    v.extend(crate::stamp_cmds::specs());
    v.extend(crate::align_cmds::specs());
    v.extend(crate::photo_cmds::specs());
    v.extend(crate::lens_cmds::specs());
    v.extend(crate::vp_cmds::specs());
    v.extend(crate::channel_cmds::specs());
    v.extend(crate::adjust_cmds::specs());
    v.extend(crate::layer_menu_cmds::specs());
    v.extend(crate::mode_cmds::specs());
    v.extend(crate::multichannel_cmds::specs());
    v.extend(crate::pattern_cmds::specs());
    v.extend(crate::warp_cmds::specs());
    v.extend(crate::comps_cmds::specs());
    v.extend(crate::artboard_cmds::specs());
    v.extend(crate::distort_cmds::specs());
    v.extend(crate::analysis_cmds::specs());
    v.extend(crate::notes_cmds::specs());
    v.extend(crate::proof_sim::specs());
    v.extend(crate::presets::specs());
    v.extend(crate::render_cmds::specs());
    v.extend(crate::slice_cmds::specs());
    v.extend(crate::web_cmds::specs());
    v.extend(crate::automate_cmds::specs());
    v.extend(crate::print_cmds::specs());
    v.extend(crate::pick_cmds::specs());
    v.extend(crate::frame_cmds::specs());
    v.extend(crate::migrate_cmds::specs());
    v.extend(crate::trap_cmds::specs());
    v.extend(crate::timeline_cmds::specs());
    v.extend(crate::video_cmds::specs());
    v.extend(crate::jobs::specs());
    v.extend(crate::wia_cmds::specs());
    v.extend(crate::variables_cmds::specs());
    v.extend(crate::plugin_cmds::specs());
    v.extend(crate::group_view_cmds::specs());
    v.extend(crate::fx_view_cmds::specs());
    v.extend(crate::mask_view_cmds::specs());
    v
}

/// Commands generated from a table share one `run` fn; the kind is recovered from the id.
pub(crate) fn inject_kind(id: &str, params: Value) -> Value {
    let kind = id.strip_prefix("layer.newAdjustmentLayer.").or_else(|| id.strip_prefix("image.adjustments."));
    match (kind, params) {
        (Some(k), Value::Object(mut m)) => {
            m.insert("__kind".into(), Value::String(k.into()));
            Value::Object(m)
        }
        (Some(k), _) => json!({ "__kind": k }),
        (None, p) => p,
    }
}

pub fn adjustment_kind(a: &Adjustment) -> &'static str {
    match a {
        Adjustment::BrightnessContrast { .. } => "brightnessContrast",
        Adjustment::Levels { .. } => "levels",
        Adjustment::Curves { .. } => "curves",
        Adjustment::Exposure { .. } => "exposure",
        Adjustment::Vibrance { .. } => "vibrance",
        Adjustment::HueSaturation { .. } => "hueSaturation",
        Adjustment::ColorBalance { .. } => "colorBalance",
        Adjustment::BlackWhite { .. } => "blackWhite",
        Adjustment::PhotoFilter { .. } => "photoFilter",
        Adjustment::ChannelMixer { .. } => "channelMixer",
        Adjustment::Posterize { .. } => "posterize",
        Adjustment::Threshold { .. } => "threshold",
        Adjustment::GradientMap { .. } => "gradientMap",
        Adjustment::SelectiveColor { .. } => "selectiveColor",
        Adjustment::ColorLookup { .. } => "colorLookup",
        _ => "invert",
    }
}

fn arrange(s: &mut Session, p: &Value, delta: i32) -> Result<Value> {
    let id = layer_param(s, p)?;
    s.edit("Arrange", |doc, _| {
        let steps = match delta {
            i32::MAX => 10_000,
            i32::MIN => -10_000,
            d => d,
        };
        let dir = steps.signum();
        let mut moved = false;
        for _ in 0..steps.abs() {
            if !doc.shift(id, dir) {
                break;
            }
            moved = true;
        }
        if moved { Ok(()) } else { Err(EngineError::Other("layer can't move further".into())) }
    })?;
    Ok(Value::Null)
}

fn set_mask(s: &mut Session, p: &Value, label: &str, mask: Option<LayerMask>) -> Result<Value> {
    let id = layer_param(s, p)?;
    s.edit(label, |doc, _| {
        if mask.is_some() {
            crate::extra_cmds::background_to_layer_for_mask(doc, id);
        }
        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.mask = mask;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn combine(a: &Surface, b: &Surface, area: Rect, f: impl Fn(f32, f32) -> f32) -> Surface {
    let ra = a.read_region(area);
    let rb = b.read_region(area);
    let data: Vec<f32> = ra.iter().zip(&rb).map(|(x, y)| f(*x, *y)).collect();
    let mut out = Surface::new(photocraft_color::PixelFormat::GRAY8);
    out.write_region(area, &data);
    out.prune();
    out
}

/// Move a layer's pixels, linked mask and type by whole pixels (vectors move via
/// `vector_cmds::translate_vectors`).
pub(crate) fn translate_layer(doc: &Document, l: &mut Layer, dx: i32, dy: i32) {
    use crate::layer_multi_cmds::shift_surface as translate_surface;
    // Linked patterns in the layer's effects move with it.
    if let Some(r) = &mut l.effects.reference {
        *r = (r.0 + f64::from(dx), r.1 + f64::from(dy));
    }
    if let Some(m) = &mut l.mask
        && m.linked
    {
        m.surface = translate_surface(&m.surface, dx, dy);
    }
    match &mut l.content {
        LayerContent::Raster(s) => *s = translate_surface(s, dx, dy),
        LayerContent::Text(t) => {
            t.transform = photocraft_geom::Affine::translate(dx as f64, dy as f64).mul(&t.transform);
            crate::type_cmds::refresh(doc, t);
        }
        LayerContent::Group(g) => {
            // Moving an artboard moves the board with its contents.
            if let Some(a) = &mut g.artboard {
                a.rect = a.rect.translate(dx, dy);
            }
            for c in &mut g.children {
                translate_layer(doc, c, dx, dy);
            }
        }
        LayerContent::Smart(sm) => crate::smart_cmds::shift_smart(sm, dx, dy),
        _ => {}
    }
}

/// Does `l` (or any descendant) have id `target`?
fn contains_layer(l: &Layer, target: LayerId) -> bool {
    l.id == target || l.children().is_some_and(|c| c.iter().any(|c| contains_layer(c, target)))
}
