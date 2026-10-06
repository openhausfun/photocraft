//! Web slices: the Slice tool (`slice.new`, Slices From Guides), the Slice Select tool (`slice.set`
//! = Slice Options, promote, divide, delete), Layer › New Layer Based Slice, View › Lock Slices and
//! View › Clear Slices.
//!
//! The document stores user and layer-based slices (`Document::slices`); auto slices are derived
//! (`photocraft_doc::slices::resolve`). Layer-based slices follow their layer: after every
//! command [`refresh`] re-fits them to the layer's bounds including layer effects, plus outsets,
//! without a history step of their own (the change is implied by the edit that moved the layer).
//! Slices are addressed by stored id (`"slice"`) or by their displayed number (`"number"`).

use std::sync::Arc;

use photocraft_doc::slices::{self, ResolvedSlice, Slice, SliceKind, SliceOrigin};
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, layer_param};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

// ---------- predicates ----------

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn unlocked(s: &Session) -> std::result::Result<(), String> {
    has_doc(s)?;
    if s.file_menu.slices_locked { Err("slices are locked (View › Lock Slices)".into()) } else { Ok(()) }
}

fn has_slices(s: &Session) -> std::result::Result<(), String> {
    unlocked(s)?;
    if s.active().is_some_and(|d| !d.doc.slices.is_empty()) { Ok(()) } else { Err("the document has no user or layer-based slices".into()) }
}

fn has_layer_for_slice(s: &Session) -> std::result::Result<(), String> {
    unlocked(s)?;
    let d = s.active().ok_or("no document open")?;
    let id = d.active_layer.ok_or("no active layer")?;
    let l = d.doc.layer(id).ok_or("no active layer")?;
    if matches!(l.content, LayerContent::Adjustment(_)) {
        return Err("adjustment layers have no bounds to slice".into());
    }
    if d.doc.layers.first().is_some_and(|b| b.id == id && b.name == "Background" && b.locks.transparency && b.locks.position) {
        return Err("the Background layer can't have a layer-based slice".into());
    }
    if d.doc.slices.list.iter().any(|s| s.layer == Some(id)) {
        return Err("the layer already has a layer-based slice".into());
    }
    Ok(())
}

// ---------- geometry ----------

/// Bounds of a layer's pixels plus its layer effects (groups: the union of their children).
pub fn layer_bounds(l: &Layer) -> Rect {
    let own = match &l.content {
        LayerContent::Group(g) => match &g.artboard {
            Some(a) => a.rect,
            None => g.children.iter().filter(|c| c.visible).map(layer_bounds).fold(Rect::EMPTY, |a, b| {
                if a.is_empty() {
                    b
                } else if b.is_empty() {
                    a
                } else {
                    a.union(&b)
                }
            }),
        },
        _ => photocraft_doc::comps::position_bounds(l),
    };
    if own.is_empty() || !photocraft_compose::effects::has_effects(l) {
        return own;
    }
    own.inflate(photocraft_compose::effects::margin(l))
}

fn layer_slice_rect(doc: &Document, s: &Slice) -> Option<Rect> {
    let l = doc.layer(s.layer?)?;
    let b = layer_bounds(l);
    let [t, le, bo, r] = s.outsets;
    Some(if b.is_empty() { b } else { Rect::new(b.x0 - le, b.y0 - t, b.x1 + r, b.y1 + bo) })
}

/// Re-fits layer-based slices of the active document to their layers; drops slices whose layer
/// was deleted (as Photoshop does). Not a history step.
pub(crate) fn refresh(s: &mut Session) {
    let Some(st) = s.active_mut() else { return };
    if !st.doc.slices.list.iter().any(|sl| sl.origin == SliceOrigin::Layer) {
        return;
    }
    let doc = &st.doc;
    let mut changes: Vec<(u32, Option<Rect>)> = Vec::new();
    for sl in doc.slices.list.iter().filter(|sl| sl.origin == SliceOrigin::Layer) {
        let r = layer_slice_rect(doc, sl);
        if r != Some(sl.rect) {
            changes.push((sl.id, r));
        }
    }
    if changes.is_empty() {
        return;
    }
    let doc = Arc::make_mut(&mut st.doc);
    for (id, r) in changes {
        match r {
            Some(r) => {
                if let Some(sl) = doc.slices.get_mut(id) {
                    sl.rect = r;
                }
            }
            None => doc.slices.list.retain(|sl| sl.id != id),
        }
    }
}

// ---------- params ----------

pub(crate) fn rect_param(p: &Value) -> Option<Rect> {
    let f = |v: &Value| v.as_f64().filter(|x| x.is_finite()).map(|x| x.round() as i32);
    if let Some(a) = p.get("rect").and_then(Value::as_array).filter(|a| a.len() == 4) {
        let v: Vec<i32> = a.iter().filter_map(f).collect();
        if v.len() == 4 && v[2] > 0 && v[3] > 0 {
            return Some(Rect::new(v[0], v[1], v[0] + v[2], v[1] + v[3]));
        }
        return None;
    }
    let (x, y, w, h) = (f(p.get("x")?)?, f(p.get("y")?)?, f(p.get("width")?)?, f(p.get("height")?)?);
    (w > 0 && h > 0).then(|| Rect::new(x, y, x + w, y + h))
}

fn hex_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
    match s.len() {
        6 => Some([255, b(0)?, b(2)?, b(4)?]),
        _ => None,
    }
}

/// Slice Options fields present in `p` applied to `sl`.
fn apply_options(sl: &mut Slice, p: &Value, cmd: &str) -> Result<()> {
    let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
    if let Some(v) = text("name") {
        sl.name = v;
    }
    if let Some(v) = text("kind").or_else(|| text("type")) {
        sl.kind = SliceKind::from_id(&v).ok_or_else(|| bad(cmd, format!("unknown slice type `{v}` (image|noImage|table)")))?;
    }
    for (k, slot) in [("url", &mut sl.url), ("target", &mut sl.target), ("message", &mut sl.message), ("alt", &mut sl.alt), ("cellText", &mut sl.cell_text)] {
        if let Some(v) = text(k) {
            *slot = v;
        }
    }
    if let Some(b) = p.get("cellTextIsHtml").and_then(Value::as_bool) {
        sl.cell_text_is_html = b;
    }
    for (k, slot) in [("horizontalAlign", &mut sl.horizontal_align), ("verticalAlign", &mut sl.vertical_align)] {
        if let Some(v) = crate::commands::int(p, k).filter(|v| *v >= 0).map(|v| v as u64) {
            *slot = v.min(4) as u32;
        }
    }
    match p.get("background").and_then(Value::as_str) {
        Some("none") | Some("") => sl.background = None,
        Some(c) => sl.background = Some(hex_color(c).ok_or_else(|| bad(cmd, "background is \"none\" or \"#rrggbb\""))?),
        None => {}
    }
    if let Some(a) = p.get("outsets").and_then(Value::as_array).filter(|a| a.len() == 4) {
        for (i, v) in a.iter().enumerate() {
            sl.outsets[i] = v.as_i64().unwrap_or(0).clamp(-10_000, 10_000) as i32;
        }
    }
    if let Some(r) = rect_param(p) {
        if sl.origin == SliceOrigin::Layer {
            // Editing a layer-based slice's geometry promotes it to a user slice.
            sl.origin = SliceOrigin::User;
            sl.layer = None;
        }
        sl.rect = r;
    }
    Ok(())
}

/// The stored slice addressed by `"slice"` (id) or `"number"`; an auto slice's number promotes
/// it to a user slice first when `promote` is set.
fn target(s: &mut Session, p: &Value, cmd: &str, promote: bool) -> Result<u32> {
    if let Some(id) = crate::commands::int(p, "slice").filter(|v| *v >= 0).map(|v| v as u64) {
        let d = s.active().ok_or(EngineError::NoDocument)?;
        return d.doc.slices.get(id as u32).map(|sl| sl.id).ok_or_else(|| bad(cmd, format!("no slice with id {id}")));
    }
    let n = crate::commands::int(p, "number").filter(|v| *v > 0).ok_or_else(|| bad(cmd, "give \"slice\" (id) or \"number\""))? as usize;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let r = slices::resolve(&d.doc).into_iter().find(|r| r.number == n).ok_or_else(|| bad(cmd, format!("no slice number {n}")))?;
    match r.id {
        Some(id) => Ok(id),
        None if promote => {
            let rect = r.rect;
            s.edit("Promote to User Slice", |doc, _| {
                let id = doc.slices.next_id();
                doc.slices.list.push(Slice { id, rect, ..Default::default() });
                Ok(id)
            })
        }
        None => Err(bad(cmd, format!("slice {n} is an auto slice"))),
    }
}

pub fn resolved_json(doc: &Document, r: &ResolvedSlice) -> Value {
    let s = r.id.and_then(|id| doc.slices.get(id));
    json!({
        "number": r.number,
        "id": r.id,
        "origin": r.origin.id(),
        "name": r.name,
        "rect": [r.rect.x0, r.rect.y0, r.rect.width(), r.rect.height()],
        "kind": r.kind.id(),
        "layer": s.and_then(|s| s.layer).map(|l| l.0),
        "url": s.map_or("", |s| s.url.as_str()),
        "target": s.map_or("", |s| s.target.as_str()),
        "message": s.map_or("", |s| s.message.as_str()),
        "alt": s.map_or("", |s| s.alt.as_str()),
        "cellText": s.map_or("", |s| s.cell_text.as_str()),
        "background": s.and_then(|s| s.background).map(|c| format!("#{:02x}{:02x}{:02x}", c[1], c[2], c[3])),
    })
}

// ---------- commands ----------

fn new_slice(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "slice.new";
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let rect = rect_param(p).ok_or_else(|| bad(cmd, "give \"rect\":[x,y,w,h] (or x, y, width, height)"))?.intersect(&d.doc.bounds());
    if rect.is_empty() {
        return Err(bad(cmd, "the slice is outside the canvas"));
    }
    let id = s.edit("Slice", |doc, _| {
        let id = doc.slices.next_id();
        let mut sl = Slice { id, rect, ..Default::default() };
        apply_options(&mut sl, p, cmd)?;
        sl.rect = rect;
        doc.slices.list.push(sl);
        Ok(id)
    })?;
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let number = slices::resolve(doc).iter().find(|r| r.id == Some(id)).map(|r| r.number);
    Ok(json!({"slice": id, "number": number}))
}

fn from_guides(s: &mut Session) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let b = d.doc.bounds();
    let mut xs: Vec<i32> = vec![b.x0, b.x1];
    let mut ys: Vec<i32> = vec![b.y0, b.y1];
    xs.extend(d.doc.guides.vertical.iter().map(|g| (*g as f64).round() as i32).filter(|x| *x > b.x0 && *x < b.x1));
    ys.extend(d.doc.guides.horizontal.iter().map(|g| (*g as f64).round() as i32).filter(|y| *y > b.y0 && *y < b.y1));
    if xs.len() == 2 && ys.len() == 2 {
        return Err(EngineError::Other("the document has no guides inside the canvas".into()));
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    let mut rects = Vec::new();
    for yw in ys.windows(2) {
        for xw in xs.windows(2) {
            rects.push(Rect::new(xw[0], yw[0], xw[1], yw[1]));
        }
    }
    let n = rects.len();
    s.edit("Slices From Guides", |doc, _| {
        // Photoshop replaces every existing slice.
        doc.slices.list.clear();
        for r in rects {
            let id = doc.slices.next_id();
            doc.slices.list.push(Slice { id, rect: r, ..Default::default() });
        }
        Ok(())
    })?;
    Ok(json!({"slices": n}))
}

fn set_slice(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "slice.set";
    let id = target(s, p, cmd, true)?;
    let coalesce_into_promote = s.active().is_some_and(|d| d.history.undo_label() == Some("Promote to User Slice")) && p.get("number").is_some();
    let label = "Slice Options";
    let run = |doc: &mut Document, _: &mut Option<LayerId>| -> Result<()> {
        let sl = doc.slices.get_mut(id).ok_or_else(|| bad(cmd, "slice vanished"))?;
        apply_options(sl, p, cmd)?;
        Ok(())
    };
    if coalesce_into_promote {
        // Promote + options are one user action: fold the options into the promote step.
        let st = s.active_mut().ok_or(EngineError::NoDocument)?;
        let mut doc = (*st.doc).clone();
        let mut a = st.active_layer;
        run(&mut doc, &mut a)?;
        st.doc = Arc::new(doc);
        st.revision += 1;
        st.last_damage = None;
    } else {
        s.edit(label, run)?;
    }
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let r = slices::resolve(&d.doc).into_iter().find(|r| r.id == Some(id)).ok_or_else(|| bad(cmd, "slice is off the canvas"))?;
    Ok(resolved_json(&d.doc, &r))
}

fn promote(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "slice.promote";
    let id = target(s, p, cmd, true)?;
    // Layer-based slices promote too: they stop following their layer.
    let is_layer = s.active().and_then(|d| d.doc.slices.get(id)).is_some_and(|sl| sl.origin == SliceOrigin::Layer);
    if is_layer {
        s.edit("Promote to User Slice", |doc, _| {
            if let Some(sl) = doc.slices.get_mut(id) {
                sl.origin = SliceOrigin::User;
                sl.layer = None;
            }
            Ok(())
        })?;
    }
    Ok(json!({"slice": id}))
}

fn ids_param(s: &mut Session, p: &Value, cmd: &str) -> Result<Vec<u32>> {
    if let Some(a) = p.get("slices").and_then(Value::as_array) {
        let d = s.active().ok_or(EngineError::NoDocument)?;
        let ids: Vec<u32> = a.iter().filter_map(Value::as_u64).map(|v| v as u32).filter(|id| d.doc.slices.get(*id).is_some()).collect();
        if ids.is_empty() {
            return Err(bad(cmd, "none of the slices exist"));
        }
        return Ok(ids);
    }
    Ok(vec![target(s, p, cmd, false)?])
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = ids_param(s, p, "slice.delete")?;
    let n = ids.len();
    s.edit("Delete Slice", |doc, _| {
        doc.slices.list.retain(|sl| !ids.contains(&sl.id));
        Ok(())
    })?;
    Ok(json!({"deleted": n}))
}

fn divide(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "slice.divide";
    let id = target(s, p, cmd, true)?;
    let down = crate::commands::int(p, "horizontal").unwrap_or(1).clamp(1, 1000) as i32;
    let across = crate::commands::int(p, "vertical").unwrap_or(1).clamp(1, 1000) as i32;
    if down == 1 && across == 1 {
        return Err(bad(cmd, "give \"horizontal\" (slices down) and/or \"vertical\" (slices across) > 1"));
    }
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let src = d.doc.slices.get(id).cloned().ok_or_else(|| bad(cmd, "no such slice"))?;
    let r = src.rect;
    if (r.width() as i32) < across || (r.height() as i32) < down {
        return Err(bad(cmd, "the slice is too small to divide that many times"));
    }
    let n = (down * across) as usize;
    let ids = s.edit("Divide Slice", |doc, _| {
        let pos = doc.slices.list.iter().position(|sl| sl.id == id).unwrap_or(0);
        doc.slices.list.retain(|sl| sl.id != id);
        let mut ids = Vec::new();
        let mut parts = Vec::new();
        for j in 0..down {
            for i in 0..across {
                let x0 = r.x0 + (r.width() as i32 * i) / across;
                let x1 = r.x0 + (r.width() as i32 * (i + 1)) / across;
                let y0 = r.y0 + (r.height() as i32 * j) / down;
                let y1 = r.y0 + (r.height() as i32 * (j + 1)) / down;
                let nid = if ids.is_empty() { id } else { doc.slices.next_id().max(ids.iter().copied().max().unwrap_or(0) + 1) };
                ids.push(nid);
                // The first part keeps the original's options; the others start fresh.
                let base = if parts.is_empty() { Slice { origin: SliceOrigin::User, layer: None, ..src.clone() } } else { Slice::default() };
                parts.push(Slice { id: nid, rect: Rect::new(x0, y0, x1, y1), ..base });
            }
        }
        for (k, sl) in parts.into_iter().enumerate() {
            doc.slices.list.insert((pos + k).min(doc.slices.list.len()), sl);
        }
        Ok(ids)
    })?;
    Ok(json!({"slices": ids, "count": n}))
}

fn list(s: &mut Session) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let all: Vec<Value> = slices::resolve(&d.doc).iter().map(|r| resolved_json(&d.doc, r)).collect();
    Ok(json!({"slices": all, "locked": s.file_menu.slices_locked}))
}

fn layer_based(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "layer.newLayerBasedSlice";
    let layer = layer_param(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let l = d.doc.layer(layer).ok_or(EngineError::NoLayer(layer))?;
    if d.doc.slices.list.iter().any(|sl| sl.layer == Some(layer)) {
        return Err(bad(cmd, "the layer already has a layer-based slice"));
    }
    let rect = layer_bounds(l);
    if rect.is_empty() {
        return Err(EngineError::Other("Could not create a layer based slice because the layer is empty".into()));
    }
    let id = s.edit("New Layer Based Slice", |doc, _| {
        let id = doc.slices.next_id();
        let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        doc.slices.list.push(Slice {
            id,
            origin: SliceOrigin::Layer,
            layer: Some(layer),
            rect,
            name: text("name"),
            url: text("url"),
            alt: text("alt"),
            ..Default::default()
        });
        Ok(id)
    })?;
    Ok(json!({"slice": id, "rect": [rect.x0, rect.y0, rect.width(), rect.height()]}))
}

fn lock(s: &mut Session, p: &Value) -> Result<Value> {
    let on = p.get("on").and_then(Value::as_bool).unwrap_or(!s.file_menu.slices_locked);
    s.file_menu.slices_locked = on;
    Ok(json!({"locked": on}))
}

fn clear(s: &mut Session) -> Result<Value> {
    let n = s.active().map_or(0, |d| d.doc.slices.list.len());
    s.edit("Clear Slices", |doc, _| {
        doc.slices.list.clear();
        Ok(())
    })?;
    Ok(json!({"cleared": n}))
}

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: None, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    vec![
        spec!(
            "slice.new",
            "Slice Tool",
            &[],
            r##"{"rect":[x,y,w,h] | "x","y","width","height", plus Slice Options ("name","kind":"image|noImage|table","url","target","message","alt","cellText","cellTextIsHtml","background":"none|#rrggbb")?} → {slice, number}"##,
            unlocked,
            new_slice
        ),
        spec!("slice.fromGuides", "Slices From Guides", &[], "{} (replaces every slice by the grid of the canvas guides) → {slices}", unlocked, |s, _| {
            from_guides(s)
        }),
        spec!(
            "slice.set",
            "Slice Options…",
            &[],
            r##"{"slice":id | "number":n (an auto slice is promoted), "name"?,"kind":"image|noImage|table"?,"url"?,"target"?,"message"?,"alt"?,"cellText"?,"cellTextIsHtml"?,"horizontalAlign":0..4?,"verticalAlign":0..4?,"background":"none|#rrggbb"?,"outsets":[t,l,b,r]? (layer slices),"rect":[x,y,w,h]? (move/resize; a layer slice becomes a user slice)} → the slice"##,
            unlocked,
            set_slice
        ),
        spec!("slice.promote", "Promote", &[], r##"{"slice":id | "number":n} (auto or layer-based → user slice) → {slice}"##, unlocked, promote),
        spec!("slice.delete", "Delete Slice", &[], r##"{"slice":id | "number":n | "slices":[id…]} → {deleted}"##, has_slices, delete),
        spec!(
            "slice.divide",
            "Divide Slice…",
            &[],
            r##"{"slice":id | "number":n,"horizontal":n=1 (slices down),"vertical":n=1 (slices across)} → {slices}"##,
            unlocked,
            divide
        ),
        CommandSpec {
            id: "slice.list",
            label: "List Slices",
            menu: &[],
            shortcut: None,
            params: "{} → {slices:[{number,id,origin:auto|layer|user,name,rect:[x,y,w,h],kind,layer,url,alt,…}], locked}",
            enabled: has_doc,
            journal: false,
            run: |s, _| list(s),
        },
        spec!(
            "layer.newLayerBasedSlice",
            "New Layer Based Slice",
            &["Layer"],
            r##"{"layer":id?,"name":str?,"url":str?,"alt":str?} (follows the layer's bounds with effects) → {slice, rect}"##,
            has_layer_for_slice,
            layer_based
        ),
        spec!("view.lockSlices", "Lock Slices", &["View"], r##"{"on":bool? (default: toggle)} → {locked}"##, has_doc, lock),
        spec!("view.clearSlices", "Clear Slices", &["View"], "{} (deletes every user and layer-based slice) → {cleared}", has_slices, |s, _| clear(s)),
    ]
}

#[cfg(test)]
#[path = "slice_cmds/tests.rs"]
mod tests;
