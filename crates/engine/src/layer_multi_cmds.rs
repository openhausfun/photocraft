//! Multi-layer selection and the commands that act on it: ⌘/⇧-click selection, Select › All /
//! Deselect / Find Layers, Layer › Align and Distribute, Link Layers, Merge Layers, Group from
//! Layers, Arrange › Reverse, Lock Layers and Rename Layer.
//!
//! The selection itself lives in [`DocState::selected_layers`] (UI state, like the active layer):
//! changing it never adds a history step. Every structural change below is one history step.

use photocraft_color::PixelFormat;
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, int};
use crate::{DocState, EngineError, Result, Session};

// ---------- selection state ----------

fn doc_state(s: &Session) -> std::result::Result<&DocState, String> {
    s.active().ok_or_else(|| "no document open".into())
}

/// Selected layers of the active document, bottom-to-top.
pub fn selected(s: &Session) -> Vec<LayerId> {
    s.active().map(DocState::selected_layers).unwrap_or_default()
}

/// Ids in `ids` that have no ancestor also in `ids` (a selected group already carries its
/// selected children), bottom-to-top.
pub(crate) fn top_level(doc: &Document, ids: &[LayerId]) -> Vec<LayerId> {
    let walk = doc.walk();
    let paths: Vec<_> = walk.iter().filter(|(_, _, l)| ids.contains(&l.id)).map(|(p, _, l)| (p.clone(), l.id)).collect();
    paths.iter().filter(|(p, _)| !paths.iter().any(|(q, _)| q.len() < p.len() && p.starts_with(q))).map(|(_, id)| *id).collect()
}

/// Replace the layer selection without a history step (like [`Session::select_layer`]).
pub fn set_selection(s: &mut Session, ids: Vec<LayerId>, active: Option<LayerId>, anchor: Option<LayerId>) -> Result<()> {
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    if let Some(bad) = ids.iter().chain(active.iter()).find(|id| st.doc.layer(**id).is_none()) {
        return Err(EngineError::NoLayer(*bad));
    }
    let active = active.or_else(|| ids.last().copied());
    st.selected_layers = ids;
    if let Some(a) = active
        && !st.selected_layers.contains(&a)
    {
        st.selected_layers.push(a);
    }
    st.active_layer = active;
    st.layer_anchor = anchor.or(active);
    crate::fix_selection(st);
    // Selecting layers is not an edit: keep a clean document clean.
    let clean = st.saved_revision == st.revision;
    st.revision += 1;
    if clean {
        st.saved_revision = st.revision;
    }
    st.last_damage = Some(Rect::EMPTY);
    Ok(())
}

/// After a structural edit, make `ids` the selection (dropping any that no longer exist).
fn reselect(s: &mut Session, ids: Vec<LayerId>, active: Option<LayerId>) {
    if let Some(st) = s.active_mut() {
        st.selected_layers = ids;
        if let Some(a) = active {
            st.active_layer = Some(a);
            st.layer_anchor = Some(a);
        }
        crate::fix_selection(st);
    }
}

/// `layer.select`: `{"layer": id, "mode": "replace|toggle|range|add"}`.
pub fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let id = LayerId(p.get("layer").and_then(Value::as_u64).ok_or_else(|| bad("layer.select", "missing `layer`"))?);
    let mode = p.get("mode").and_then(Value::as_str).unwrap_or("replace");
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.layer(id).is_none() {
        return Err(EngineError::NoLayer(id));
    }
    let mut sel = st.selected_layers();
    match mode {
        "replace" => s.select_layer(id)?,
        "add" => {
            if !sel.contains(&id) {
                sel.push(id);
            }
            set_selection(s, sel, Some(id), Some(id))?;
        }
        "toggle" => {
            if sel.contains(&id) {
                if sel.len() == 1 {
                    // Photoshop keeps at least the clicked layer targeted.
                    return Ok(json!({"selected": [id.0]}));
                }
                sel.retain(|x| *x != id);
                let active = if st.active_layer == Some(id) { sel.last().copied() } else { st.active_layer };
                set_selection(s, sel, active, active)?;
            } else {
                sel.push(id);
                set_selection(s, sel, Some(id), Some(id))?;
            }
        }
        "range" => {
            let anchor = st.layer_anchor.filter(|a| st.doc.layer(*a).is_some()).or(st.active_layer).unwrap_or(id);
            let order: Vec<LayerId> = st.doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
            let (a, b) = (order.iter().position(|x| *x == anchor).unwrap_or(0), order.iter().position(|x| *x == id).unwrap_or(0));
            let range = order[a.min(b)..=a.max(b)].to_vec();
            set_selection(s, range, Some(id), Some(anchor))?;
        }
        other => return Err(bad("layer.select", format!("unknown mode `{other}`"))),
    }
    Ok(json!({"selected": selected(s).iter().map(|l| l.0).collect::<Vec<_>>()}))
}

fn is_locked_background(l: &Layer) -> bool {
    l.name == "Background" && l.locks.transparency && matches!(l.content, LayerContent::Raster(_))
}

fn select_all_layers(s: &mut Session) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let ids: Vec<LayerId> = d.doc.walk().into_iter().filter(|(_, _, l)| !is_locked_background(l)).map(|(_, _, l)| l.id).collect();
    if ids.is_empty() {
        return Err(EngineError::Other("there are no layers to select".into()));
    }
    let active = d.active_layer.filter(|a| ids.contains(a)).or(ids.last().copied());
    let n = ids.len();
    set_selection(s, ids, active, None)?;
    Ok(json!({"selected": n}))
}

fn find_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let needle = p.get("name").and_then(Value::as_str).ok_or_else(|| bad("select.findLayers", "missing `name`"))?.to_lowercase();
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let ids: Vec<LayerId> = d.doc.walk().into_iter().filter(|(_, _, l)| l.name.to_lowercase().contains(&needle)).map(|(_, _, l)| l.id).collect();
    if ids.is_empty() {
        return Err(EngineError::Other(format!("no layer name contains \"{needle}\"")));
    }
    let out = ids.iter().map(|l| l.0).collect::<Vec<_>>();
    set_selection(s, ids, None, None)?;
    Ok(json!({"selected": out}))
}

fn select_linked(s: &mut Session) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let groups = link_groups(&d.doc, &d.selected_layers());
    let ids: Vec<LayerId> = d.doc.walk().into_iter().filter(|(_, _, l)| l.link_group.is_some_and(|g| groups.contains(&g))).map(|(_, _, l)| l.id).collect();
    let active = d.active_layer;
    let n = ids.len();
    set_selection(s, ids, active, active)?;
    Ok(json!({"selected": n}))
}

// ---------- enabled predicates ----------

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    if selected(s).is_empty() { Err("no layer selected".into()) } else { Ok(()) }
}
fn has_doc(s: &Session) -> std::result::Result<(), String> {
    doc_state(s).map(|_| ())
}
fn two_plus(s: &Session) -> std::result::Result<(), String> {
    if selected(s).len() >= 2 { Ok(()) } else { Err("select two or more layers".into()) }
}
fn can_align(s: &Session) -> std::result::Result<(), String> {
    let d = doc_state(s)?;
    match d.selected_layers().len() {
        0 => Err("no layer selected".into()),
        // Photoshop aligns a single layer to the canvas (or to an active selection); 2+ align to each other.
        _ => Ok(()),
    }
}
fn can_distribute(s: &Session) -> std::result::Result<(), String> {
    if selected(s).len() >= 3 { Ok(()) } else { Err("select three or more layers to distribute".into()) }
}
fn can_link(s: &Session) -> std::result::Result<(), String> {
    let d = doc_state(s)?;
    let sel = d.selected_layers();
    match sel.len() {
        0 => Err("no layer selected".into()),
        1 if d.doc.layer(sel[0]).is_none_or(|l| l.link_group.is_none()) => Err("select two or more layers to link".into()),
        _ => Ok(()),
    }
}
fn has_linked(s: &Session) -> std::result::Result<(), String> {
    let d = doc_state(s)?;
    if link_groups(&d.doc, &d.selected_layers()).is_empty() { Err("the selected layers are not linked".into()) } else { Ok(()) }
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

// ---------- moving layers ----------

fn link_groups(doc: &Document, ids: &[LayerId]) -> Vec<u64> {
    let mut g: Vec<u64> = ids.iter().filter_map(|id| doc.layer(*id)?.link_group).collect();
    g.sort_unstable();
    g.dedup();
    g
}

/// `ids` plus every layer linked to one of them.
pub(crate) fn with_links(doc: &Document, ids: &[LayerId]) -> Vec<LayerId> {
    let groups = link_groups(doc, ids);
    let mut out = ids.to_vec();
    if !groups.is_empty() {
        for (_, _, l) in doc.walk() {
            if l.link_group.is_some_and(|g| groups.contains(&g)) && !out.contains(&l.id) {
                out.push(l.id);
            }
        }
    }
    out
}

/// Move layers by whole pixels the way the Move tool does: pixels, linked masks, type and
/// vector content (shapes, linked vector masks) all move together.
pub(crate) fn move_layers(doc: &mut Document, moves: &[(LayerId, i32, i32)]) -> Result<()> {
    let snapshot = doc.clone();
    for &(id, dx, dy) in moves {
        if dx == 0 && dy == 0 {
            continue;
        }
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.locks.position || l.locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is position-locked", l.name)));
        }
        crate::commands::translate_layer(&snapshot, l, dx, dy);
        crate::vector_cmds::translate_vectors(&snapshot, l, dx as f64, dy as f64);
    }
    Ok(())
}

/// `layer.translate`: the explicit layer, or every selected layer, plus their linked layers.
pub fn translate(s: &mut Session, p: &Value) -> Result<Value> {
    let dx = int(p, "dx").unwrap_or(0) as i32;
    let dy = int(p, "dy").unwrap_or(0) as i32;
    if dx == 0 && dy == 0 {
        return Ok(Value::Null);
    }
    let roots = match p.get("layer").and_then(Value::as_u64) {
        Some(id) => vec![LayerId(id)],
        None => selected(s),
    };
    if roots.is_empty() {
        return Err(EngineError::Other("no active layer".into()));
    }
    let before = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let ids = s.edit("Move", |doc, _| {
        let ids = move_targets(doc, &roots);
        if ids.is_empty() {
            return Err(EngineError::NoLayer(roots[0]));
        }
        let moves: Vec<_> = ids.iter().map(|&id| (id, dx, dy)).collect();
        move_layers(doc, &moves)?;
        Ok(ids)
    })?;
    note_damage(s, &before, &ids);
    Ok(Value::Null)
}

/// The layers a move of `roots` takes along: their linked layers, without layers whose group
/// is also moving (the group carries them).
pub fn move_targets(doc: &Document, roots: &[LayerId]) -> Vec<LayerId> {
    top_level(doc, &with_links(doc, roots))
}

/// `doc` with the layers `ids` ([`move_targets`]) moved by whole pixels, as `layer.translate`
/// leaves them: the Move tool's live preview while it drags. Type, shape and smart-object pixels
/// shift as they are instead of re-rendering (the commit re-renders them), so a drag frame costs
/// copying the moving pixels only.
pub fn moved(doc: &Document, ids: &[LayerId], dx: i32, dy: i32) -> Result<Document> {
    let mut out = doc.clone();
    for &id in ids {
        let l = out.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.locks.position || l.locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is position-locked", l.name)));
        }
        shift_shown(doc, l, dx, dy);
    }
    Ok(out)
}

/// `photocraft_algo::resample::translate_surface` with the content scan cached per tile (a Move
/// drag shifts the same layers every frame; scanning them each time cost more than the copy).
pub(crate) fn shift_surface(s: &photocraft_raster::Surface, dx: i32, dy: i32) -> photocraft_raster::Surface {
    let mut out = photocraft_raster::Surface::with_default(s.format(), &s.default_pixel());
    let r = photocraft_compose::bounds::content_bounds(s);
    if !r.is_empty() {
        out.write_interleaved(r.translate(dx, dy), &s.to_interleaved(r));
    }
    out
}

/// [`crate::commands::translate_layer`] plus the vector side, without re-rendering anything.
fn shift_shown(doc: &Document, l: &mut Layer, dx: i32, dy: i32) {
    use self::shift_surface as translate_surface;
    let a = photocraft_geom::Affine::translate(f64::from(dx), f64::from(dy));
    if let Some(r) = &mut l.effects.reference {
        *r = (r.0 + f64::from(dx), r.1 + f64::from(dy));
    }
    if let Some(m) = l.mask.as_mut().filter(|m| m.linked) {
        m.surface = translate_surface(&m.surface, dx, dy);
    }
    if let Some(vm) = l.vector_mask.as_mut().filter(|v| v.linked) {
        vm.path = vm.path.transform(&a);
    }
    match &mut l.content {
        LayerContent::Raster(s) => *s = translate_surface(s, dx, dy),
        LayerContent::Text(t) => {
            t.transform = a.mul(&t.transform);
            if let Some(c) = &mut t.cache {
                *c = translate_surface(c, dx, dy);
            }
        }
        LayerContent::Shape(sh) => {
            // The rendered shape is cut at the canvas: one that reaches past it renders again.
            let canvas = doc.bounds();
            let inside = sh.cache.as_ref().is_some_and(|c| {
                let b = photocraft_compose::bounds::content_bounds(c);
                b.is_empty() || (b.x0 > canvas.x0 && b.y0 > canvas.y0 && b.x1 < canvas.x1 && b.y1 < canvas.y1)
            });
            crate::vector_cmds::transform_shape(sh, &a);
            match &mut sh.cache {
                Some(c) if inside => *c = translate_surface(c, dx, dy),
                _ => crate::vector_cmds::refresh_shape(doc, sh),
            }
        }
        LayerContent::Smart(sm) => crate::smart_cmds::shift_smart(sm, dx, dy),
        LayerContent::Group(g) => {
            if let Some(ab) = &mut g.artboard {
                ab.rect = ab.rect.translate(dx, dy);
            }
            g.children.iter_mut().for_each(|c| shift_shown(doc, c, dx, dy));
        }
        LayerContent::Adjustment(_) | LayerContent::Fill(_) => {}
    }
}

/// The document pixels an edit of the layers `ids` (moved, shown, hidden or restyled; contents
/// otherwise as they were) can have changed from `before` to `after`, or `None` for anywhere.
pub fn layers_damage(before: &Document, after: &Document, ids: &[LayerId]) -> Option<Rect> {
    let mut out = Rect::EMPTY;
    for d in [before, after] {
        let canvas = d.bounds();
        for id in ids {
            // A layer gone from one side (never for a move) changes who knows what.
            let b = photocraft_compose::change_bounds(d.layer(*id)?, canvas)?;
            if !b.is_empty() {
                out = if out.is_empty() { b } else { out.union(&b) };
            }
        }
    }
    Some(out)
}

/// Report [`layers_damage`] as the active document's last change, so the canvas recomposites
/// only that (grown by the effect reach around it) instead of everything.
pub(crate) fn note_damage(s: &mut Session, before: &Document, ids: &[LayerId]) {
    let Some(st) = s.active_mut() else { return };
    if before.size != st.doc.size {
        return;
    }
    st.last_damage = layers_damage(before, &st.doc, ids);
}

/// Content bounds used by Align/Distribute: the layer's pixels (type and shape layers use their
/// rendered pixels), or the union of a group's children. Fill and adjustment layers have none.
pub fn layer_bounds(l: &Layer) -> Option<Rect> {
    let r = match &l.content {
        LayerContent::Group(g) => g.children.iter().filter_map(layer_bounds).fold(Rect::EMPTY, |a, b| a.union(&b)),
        _ => l.surface().map(|s| s.content_bounds()).unwrap_or(Rect::EMPTY),
    };
    (!r.is_empty()).then_some(r)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    Top,
    VCenter,
    Bottom,
    Left,
    HCenter,
    Right,
}

impl Edge {
    fn parse(k: &str) -> Option<Edge> {
        Some(match k {
            "topEdges" => Edge::Top,
            "verticalCenters" => Edge::VCenter,
            "bottomEdges" => Edge::Bottom,
            "leftEdges" => Edge::Left,
            "horizontalCenters" => Edge::HCenter,
            "rightEdges" => Edge::Right,
            _ => return None,
        })
    }
    fn vertical(self) -> bool {
        matches!(self, Edge::Top | Edge::VCenter | Edge::Bottom)
    }
    /// The edge's coordinate, doubled so centres stay integral.
    fn value2(self, r: &Rect) -> i64 {
        let (a, b) = if self.vertical() { (r.y0, r.y1) } else { (r.x0, r.x1) };
        match self {
            Edge::Top | Edge::Left => 2 * a as i64,
            Edge::Bottom | Edge::Right => 2 * b as i64,
            Edge::VCenter | Edge::HCenter => a as i64 + b as i64,
        }
    }
    fn delta(self, d: i32) -> (i32, i32) {
        if self.vertical() { (0, d) } else { (d, 0) }
    }
}

/// Selected top-level layers that have content bounds, bottom-to-top.
fn bounded_selection(doc: &Document, sel: &[LayerId]) -> Vec<(LayerId, Rect)> {
    top_level(doc, sel).into_iter().filter_map(|id| Some((id, layer_bounds(doc.layer(id)?)?))).collect()
}

fn align(s: &mut Session, p: &Value, kind: &str) -> Result<Value> {
    let cmd = format!("layer.align.{kind}");
    let edge = Edge::parse(kind).ok_or_else(|| bad(&cmd, "unknown edge"))?;
    let sel = selected(s);
    let to = p.get("to").and_then(Value::as_str).unwrap_or("auto").to_string();
    let moved = s.edit("Align", |doc, _| {
        let items = bounded_selection(doc, &sel);
        if items.is_empty() {
            return Err(EngineError::Other("the selected layers have no content to align".into()));
        }
        let target = match to.as_str() {
            "canvas" => doc.bounds(),
            "selection" => doc.selection.as_ref().map(|m| m.content_bounds()).ok_or_else(|| EngineError::Other("there is no selection to align to".into()))?,
            "auto" | "layers" => match (&doc.selection, sel.len()) {
                // Photoshop: one layer plus an active selection aligns to the selection bounds.
                (Some(m), 1) if to == "auto" => m.content_bounds(),
                // One layer with no selection aligns to the canvas (otherwise it would align to itself).
                (None, 1) if to == "auto" => doc.bounds(),
                _ => items.iter().fold(Rect::EMPTY, |a, (_, b)| a.union(b)),
            },
            other => return Err(bad(&cmd, format!("`to` must be auto|layers|selection|canvas, not `{other}`"))),
        };
        if target.is_empty() {
            return Err(EngineError::Other("nothing to align to".into()));
        }
        let t2 = edge.value2(&target);
        let moves: Vec<(LayerId, i32, i32)> = items
            .iter()
            .flat_map(|(id, r)| {
                let d = (t2 - edge.value2(r)).div_euclid(2) as i32;
                let (dx, dy) = edge.delta(d);
                // Linked layers travel with the layer they are linked to.
                with_links(doc, &[*id]).into_iter().map(move |l| (l, dx, dy))
            })
            .collect();
        let moves = dedup_moves(doc, moves);
        move_layers(doc, &moves)?;
        Ok(moves.iter().filter(|m| m.1 != 0 || m.2 != 0).count())
    })?;
    Ok(json!({"moved": moved}))
}

/// One move per layer (first wins), skipping layers nested in another moved layer.
fn dedup_moves(doc: &Document, moves: Vec<(LayerId, i32, i32)>) -> Vec<(LayerId, i32, i32)> {
    let mut out: Vec<(LayerId, i32, i32)> = Vec::new();
    for m in moves {
        if !out.iter().any(|o| o.0 == m.0) {
            out.push(m);
        }
    }
    let ids: Vec<LayerId> = out.iter().map(|m| m.0).collect();
    let keep = top_level(doc, &ids);
    out.retain(|m| keep.contains(&m.0));
    out
}

fn distribute(s: &mut Session, kind: &str) -> Result<Value> {
    let sel = selected(s);
    let kind = kind.to_string();
    let moved = s.edit("Distribute", |doc, _| {
        let mut items = bounded_selection(doc, &sel);
        if items.len() < 3 {
            return Err(EngineError::Other("distributing needs three or more layers with content".into()));
        }
        let n = items.len();
        let mut moves = Vec::new();
        match kind.as_str() {
            "horizontally" | "vertically" => {
                // Equal gaps between neighbours; the outermost layers stay put.
                let v = kind == "vertically";
                let lo = |r: &Rect| if v { r.y0 } else { r.x0 } as i64;
                let hi = |r: &Rect| if v { r.y1 } else { r.x1 } as i64;
                items.sort_by_key(|(_, r)| (lo(r) + hi(r), lo(r)));
                let span = hi(&items[n - 1].1) - lo(&items[0].1);
                let total: i64 = items.iter().map(|(_, r)| hi(r) - lo(r)).sum();
                let gap = (span - total) as f64 / (n - 1) as f64;
                let mut cursor = lo(&items[0].1) as f64;
                for (id, r) in &items {
                    let want = cursor.round() as i64;
                    let d = (want - lo(r)) as i32;
                    moves.push(if v { (*id, 0, d) } else { (*id, d, 0) });
                    cursor += (hi(r) - lo(r)) as f64 + gap;
                }
            }
            k => {
                let edge = Edge::parse(k).ok_or_else(|| bad("layer.distribute", format!("unknown kind `{k}`")))?;
                items.sort_by_key(|(_, r)| edge.value2(r));
                let (first, last) = (edge.value2(&items[0].1) as f64, edge.value2(&items[n - 1].1) as f64);
                for (i, (id, r)) in items.iter().enumerate() {
                    let want2 = first + (last - first) * i as f64 / (n - 1) as f64;
                    let d = ((want2 - edge.value2(r) as f64) / 2.0).round() as i32;
                    let (dx, dy) = edge.delta(d);
                    moves.push((*id, dx, dy));
                }
            }
        }
        let moves: Vec<_> = moves.into_iter().flat_map(|(id, dx, dy)| with_links(doc, &[id]).into_iter().map(move |l| (l, dx, dy))).collect();
        let moves = dedup_moves(doc, moves);
        move_layers(doc, &moves)?;
        Ok(moves.iter().filter(|m| m.1 != 0 || m.2 != 0).count())
    })?;
    Ok(json!({"moved": moved}))
}

// ---------- structural commands ----------

fn link_layers(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    let linked = s.edit("Link Layers", |doc, _| {
        let groups: Vec<Option<u64>> = sel.iter().map(|id| doc.layer(*id).and_then(|l| l.link_group)).collect();
        // Already one link group (or a single linked layer): unlink, like Photoshop's toggle.
        let unlink = groups[0].is_some() && groups.iter().all(|g| *g == groups[0]);
        let next = doc.walk().iter().filter_map(|(_, _, l)| l.link_group).max().unwrap_or(0) + 1;
        for id in &sel {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            l.link_group = if unlink { None } else { Some(next) };
        }
        // A group left with a single member is no longer a link.
        let mut counts = std::collections::HashMap::<u64, usize>::new();
        for (_, _, l) in doc.walk() {
            if let Some(g) = l.link_group {
                *counts.entry(g).or_default() += 1;
            }
        }
        let lonely: Vec<LayerId> = doc.walk().iter().filter(|(_, _, l)| l.link_group.is_some_and(|g| counts[&g] < 2)).map(|(_, _, l)| l.id).collect();
        for id in lonely {
            if let Some(l) = doc.layer_mut(id) {
                l.link_group = None;
            }
        }
        Ok(!unlink)
    })?;
    Ok(json!({"linked": linked}))
}

fn lock_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let sel = selected(s);
    let keys = ["transparency", "pixels", "position", "artboard", "all"];
    let given: Vec<(&str, bool)> = keys.iter().filter_map(|k| Some((*k, p.get(*k)?.as_bool()?))).collect();
    s.edit("Lock Layers", |doc, _| {
        // No explicit locks: toggle "lock all" (on unless every selected layer is already locked).
        let all_locked = sel.iter().all(|id| doc.layer(*id).is_some_and(|l| l.locks.all));
        let given: Vec<(&str, bool)> = if given.is_empty() { vec![("all", !all_locked)] } else { given.clone() };
        for id in &sel {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            for (k, v) in &given {
                match *k {
                    "transparency" => l.locks.transparency = *v,
                    "pixels" => l.locks.pixels = *v,
                    "position" => l.locks.position = *v,
                    "artboard" => l.locks.artboard = *v,
                    _ => l.locks.all = *v,
                }
            }
        }
        Ok(())
    })?;
    Ok(json!({"layers": sel.len()}))
}

fn rename_layer(s: &mut Session, p: &Value) -> Result<Value> {
    let name =
        p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("layer.renameLayer", "missing `name`"))?.to_string();
    let id = crate::commands::layer_param(s, p)?;
    s.edit("Rename Layer", |doc, _| {
        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.name = name;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn reverse(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    s.edit("Reverse", |doc, _| {
        let ids = top_level(doc, &sel);
        if ids.len() < 2 {
            return Err(EngineError::Other("select two or more layers to reverse".into()));
        }
        let paths: Vec<Vec<usize>> = ids.iter().map(|id| doc.path_of(*id).ok_or(EngineError::NoLayer(*id))).collect::<Result<_>>()?;
        let layers: Vec<Layer> = ids.iter().map(|id| doc.layer(*id).cloned().ok_or(EngineError::NoLayer(*id))).collect::<Result<_>>()?;
        // Slots are disjoint (no layer is nested in another), so writing whole layers back to the
        // same paths never shifts another slot.
        for (path, l) in paths.iter().zip(layers.into_iter().rev()) {
            *doc.layer_at_mut(path).ok_or_else(|| EngineError::Other("layer tree changed".into()))? = l;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Group Layers (⌘G) / Group from Layers: the explicit layer, or every selected layer, moves
/// into a new group placed where the top-most of them was. Bottom-to-top order is preserved.
pub fn group_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = match p.get("layer").and_then(Value::as_u64) {
        Some(id) => vec![LayerId(id)],
        None => selected(s),
    };
    if ids.is_empty() {
        return Err(EngineError::Other("no layer selected".into()));
    }
    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let gid = s.edit("Group Layers", |doc, active| {
        let ids = top_level(doc, &ids);
        let top = *ids.last().ok_or(EngineError::NoLayer(LayerId(0)))?;
        let name = name.unwrap_or_else(|| doc.next_layer_name("Group"));
        let gid = doc.insert_above(Some(top), Layer::group(name, vec![]));
        let mut children = Vec::with_capacity(ids.len());
        for id in &ids {
            children.push(doc.remove(*id).ok_or(EngineError::NoLayer(*id))?);
        }
        *doc.layer_mut(gid).and_then(Layer::children_mut).ok_or(EngineError::NoLayer(gid))? = children;
        *active = Some(gid);
        Ok(gid)
    })?;
    reselect(s, vec![gid], Some(gid));
    Ok(json!({ "layer": gid.0 }))
}

fn merge_layers(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    if sel.len() < 2 {
        return s.execute("layer.mergeDown", json!({}));
    }
    let mid = s.edit("Merge Layers", |doc, active| {
        let ids = top_level(doc, &sel);
        let top_id = *ids.last().ok_or_else(|| EngineError::Other("Merge Layers needs two or more layers".into()))?;
        let top = doc.layer(top_id).ok_or(EngineError::NoLayer(top_id))?.clone();
        let mut solo = doc.clone();
        solo.layers = ids.iter().filter_map(|id| doc.layer(*id)).filter(|l| l.visible).cloned().collect();
        if solo.layers.is_empty() {
            return Err(EngineError::Other("the selected layers are all hidden".into()));
        }
        let buf = photocraft_compose::flatten(&solo);
        let fmt = doc.pixel_format();
        let fmt = PixelFormat::new(fmt.mode, fmt.sample, true);
        let data: Vec<f32> = buf.px.iter().flat_map(|p| photocraft_raster::from_rgba(&fmt, *p)).collect();
        let mut merged = Layer::raster(top.name.clone(), fmt);
        merged.locks = top.locks;
        let surf = crate::pixels_mut(&mut merged)?;
        surf.write_region(doc.bounds(), &data);
        surf.prune();
        let mid = merged.id;
        // Hidden selected layers are discarded, as in Photoshop.
        for id in &ids[..ids.len() - 1] {
            doc.remove(*id);
        }
        *doc.layer_mut(top_id).ok_or(EngineError::NoLayer(top_id))? = merged;
        *active = Some(mid);
        Ok(mid)
    })?;
    reselect(s, vec![mid], Some(mid));
    Ok(json!({ "layer": mid.0 }))
}

/// Delete Layer with several layers selected.
pub fn delete_selected(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    s.edit("Delete Layers", |doc, active| {
        let ids = top_level(doc, &sel);
        let below = ids.first().and_then(|id| {
            let order: Vec<LayerId> = doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
            let i = order.iter().position(|x| x == id)?;
            order[..i].iter().rev().find(|x| !sel.contains(x)).copied()
        });
        for id in &ids {
            doc.remove(*id).ok_or(EngineError::NoLayer(*id))?;
        }
        if doc.layers.is_empty() {
            return Err(EngineError::Other("a document must keep at least one layer".into()));
        }
        *active = below.filter(|b| doc.layer(*b).is_some()).or_else(|| doc.top_layer());
        Ok(())
    })?;
    Ok(json!({"deleted": sel.len()}))
}

/// Duplicate Layer with several layers selected: each copy goes above its original and the
/// copies become the selection.
pub fn duplicate_selected(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    let old_active = s.active().and_then(|d| d.active_layer);
    let (copies, active) = s.edit("Duplicate Layers", |doc, active| {
        let mut copies = Vec::new();
        let mut new_active = None;
        for id in top_level(doc, &sel) {
            let mut dup = doc.layer(id).ok_or(EngineError::NoLayer(id))?.duplicate();
            dup.name = format!("{} copy", dup.name);
            let nid = doc.insert_above(Some(id), dup);
            if Some(id) == old_active {
                new_active = Some(nid);
            }
            copies.push(nid);
        }
        let a = new_active.or(copies.last().copied());
        *active = a;
        Ok((copies, a))
    })?;
    let out = copies.iter().map(|l| l.0).collect::<Vec<_>>();
    reselect(s, copies, active);
    Ok(json!({"layers": out}))
}

/// Show/hide every selected layer in one step.
pub fn set_visible_selected(s: &mut Session, visible: bool) -> Result<Value> {
    let sel = selected(s);
    s.edit(if visible { "Show Layers" } else { "Hide Layers" }, |doc, _| {
        for id in &sel {
            doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?.visible = visible;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Is more than one layer selected (so single-layer commands should act on the whole set)?
pub fn multi(s: &Session, p: &Value) -> bool {
    p.get("layer").is_none() && selected(s).len() > 1
}

// ---------- registry ----------

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $sc:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: $sc, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    const ALIGN: &str = r##"{"to":"auto|layers|selection|canvas"="auto"} (auto: the selection bounds with one layer and an active selection, else the selected layers' bounds)"##;
    vec![
        spec!("select.allLayers", "All Layers", &["Select"], Some("Cmd+Alt+A"), "{}", has_doc, |s, _| select_all_layers(s)),
        spec!("select.deselectLayers", "Deselect Layers", &["Select"], None, "{}", has_layer, |s, _| {
            set_selection(s, Vec::new(), None, None)?;
            if let Some(st) = s.active_mut() {
                st.active_layer = None;
                st.selected_layers.clear();
            }
            Ok(Value::Null)
        }),
        spec!("select.findLayers", "Find Layers", &["Select"], Some("Cmd+Alt+Shift+F"), r##"{"name":str (case-insensitive substring)}"##, has_doc, find_layers),
        spec!("layer.selectLinkedLayers", "Select Linked Layers", &["Layer"], None, "{}", has_linked, |s, _| select_linked(s)),
        spec!("layer.align.topEdges", "Top Edges", &["Layer", "Align"], None, ALIGN, can_align, |s, p| align(s, p, "topEdges")),
        spec!("layer.align.verticalCenters", "Vertical Centers", &["Layer", "Align"], None, ALIGN, can_align, |s, p| align(s, p, "verticalCenters")),
        spec!("layer.align.bottomEdges", "Bottom Edges", &["Layer", "Align"], None, ALIGN, can_align, |s, p| align(s, p, "bottomEdges")),
        spec!("layer.align.leftEdges", "Left Edges", &["Layer", "Align"], None, ALIGN, can_align, |s, p| align(s, p, "leftEdges")),
        spec!("layer.align.horizontalCenters", "Horizontal Centers", &["Layer", "Align"], None, ALIGN, can_align, |s, p| align(s, p, "horizontalCenters")),
        spec!("layer.align.rightEdges", "Right Edges", &["Layer", "Align"], None, ALIGN, can_align, |s, p| align(s, p, "rightEdges")),
        spec!("layer.distribute.topEdges", "Top Edges", &["Layer", "Distribute"], None, "{}", can_distribute, |s, _| distribute(s, "topEdges")),
        spec!("layer.distribute.verticalCenters", "Vertical Centers", &["Layer", "Distribute"], None, "{}", can_distribute, |s, _| distribute(
            s,
            "verticalCenters"
        )),
        spec!("layer.distribute.bottomEdges", "Bottom Edges", &["Layer", "Distribute"], None, "{}", can_distribute, |s, _| distribute(s, "bottomEdges")),
        spec!("layer.distribute.leftEdges", "Left Edges", &["Layer", "Distribute"], None, "{}", can_distribute, |s, _| distribute(s, "leftEdges")),
        spec!("layer.distribute.horizontalCenters", "Horizontal Centers", &["Layer", "Distribute"], None, "{}", can_distribute, |s, _| distribute(
            s,
            "horizontalCenters"
        )),
        spec!("layer.distribute.rightEdges", "Right Edges", &["Layer", "Distribute"], None, "{}", can_distribute, |s, _| distribute(s, "rightEdges")),
        spec!(
            "layer.distribute.horizontally",
            "Horizontally",
            &["Layer", "Distribute"],
            None,
            "{} (equal horizontal gaps)",
            can_distribute,
            |s, _| distribute(s, "horizontally")
        ),
        spec!("layer.distribute.vertically", "Vertically", &["Layer", "Distribute"], None, "{} (equal vertical gaps)", can_distribute, |s, _| distribute(
            s,
            "vertically"
        )),
        spec!("layer.linkLayers", "Link Layers", &["Layer"], None, "{} (toggles: unlinks when the selection is already one link group)", can_link, |s, _| {
            link_layers(s)
        }),
        spec!("layer.mergeLayers", "Merge Layers", &["Layer"], Some("Cmd+E"), "{} (one layer selected: Merge Down)", has_layer, |s, _| merge_layers(s)),
        spec!("layer.new.groupFromLayers", "Group from Layers…", &["Layer", "New"], None, r##"{"name":str?}"##, has_layer, group_layers),
        spec!("layer.arrange.reverse", "Reverse", &["Layer", "Arrange"], None, "{}", two_plus, |s, _| reverse(s)),
        spec!(
            "layer.lockLayers",
            "Lock Layers…",
            &["Layer"],
            None,
            r##"{"transparency":bool?,"pixels":bool?,"position":bool?,"artboard":bool?,"all":bool?} (none given: toggle lock all)"##,
            has_layer,
            lock_layers
        ),
        spec!("layer.renameLayer", "Rename Layer", &["Layer"], None, r##"{"layer":id?,"name":str}"##, has_layer, rename_layer),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(depth: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 80, "depth": depth})).unwrap();
        s
    }

    fn doc(s: &Session) -> &Document {
        &s.active().unwrap().doc
    }

    /// New pixel layer with an opaque red rectangle.
    fn rect_layer(s: &mut Session, r: Rect) -> LayerId {
        let id = LayerId(s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap());
        s.edit("paint", |doc, _| {
            doc.layer_mut(id).unwrap().surface_mut().unwrap().fill_rect(r, &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
        id
    }

    fn bounds(s: &Session, id: LayerId) -> Rect {
        layer_bounds(doc(s).layer(id).unwrap()).unwrap()
    }

    fn sel(s: &Session) -> Vec<LayerId> {
        selected(s)
    }

    fn damage(s: &Session) -> Option<Rect> {
        s.active().unwrap().last_damage
    }

    #[test]
    fn moves_and_layer_props_report_only_their_area() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(10, 10, 20, 20));
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        s.execute("layer.translate", json!({"dx": 5, "dy": -3})).unwrap();
        assert_eq!(damage(&s), Some(Rect::new(10, 7, 25, 20)), "where the layer was and is");
        s.execute("layer.setProps", json!({"layer": a.0, "visible": false})).unwrap();
        assert_eq!(damage(&s), Some(Rect::new(15, 7, 25, 17)));
        s.execute("layer.setProps", json!({"layer": a.0, "opacity": 0.5, "blend": "Multiply"})).unwrap();
        assert_eq!(damage(&s), Some(Rect::new(15, 7, 25, 17)));
        // Clipping changes reach the layers around: everything.
        s.execute("layer.setProps", json!({"layer": a.0, "clipped": true})).unwrap();
        assert_eq!(damage(&s), None);
        // An adjustment layer changes everything beneath it.
        let adj = s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.setProps", json!({"layer": adj, "visible": false})).unwrap();
        assert_eq!(damage(&s), None);
        // The Background (opaque everywhere): everything.
        let bg = doc(&s).layers[0].id;
        s.execute("layer.setProps", json!({"layer": bg.0, "opacity": 0.5})).unwrap();
        let all = doc(&s).bounds();
        assert!(damage(&s).is_none_or(|r| r.contains_rect(&all)), "{:?}", damage(&s));
        // Bad params still fail cleanly.
        assert!(s.execute("layer.setProps", json!({"layer": 999_999, "visible": true})).is_err());
        assert!(s.execute("layer.translate", json!({"layer": 999_999, "dx": 1})).is_err());
    }

    #[test]
    fn the_move_preview_matches_the_move() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(10, 10, 20, 20));
        let before = s.active().unwrap().doc.clone();
        let ids = move_targets(&before, &[a]);
        let shown = moved(&before, &ids, 7, 4).unwrap();
        s.execute("layer.translate", json!({"layer": a.0, "dx": 7, "dy": 4})).unwrap();
        assert_eq!(layer_bounds(shown.layer(a).unwrap()), Some(bounds(&s, a)));
        // Locked layers don't move in the preview either.
        let mut locked = (*before).clone();
        locked.layer_mut(a).unwrap().locks.position = true;
        assert!(moved(&locked, &ids, 1, 1).is_err());
        assert!(moved(&before, &[LayerId(999_999)], 1, 1).is_err());
    }

    fn select_all(s: &mut Session, ids: &[LayerId]) {
        s.execute("layer.select", json!({"layer": ids[0].0})).unwrap();
        for id in &ids[1..] {
            s.execute("layer.select", json!({"layer": id.0, "mode": "toggle"})).unwrap();
        }
    }

    #[test]
    fn toggle_and_range_selection() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let b = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let c = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let d = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        assert_eq!(sel(&s), vec![d]);
        s.execute("layer.select", json!({"layer": b.0, "mode": "toggle"})).unwrap();
        assert_eq!(sel(&s), vec![b, d]);
        assert_eq!(s.active().unwrap().active_layer, Some(b));
        // Toggling the active layer off hands "active" to a remaining member.
        s.execute("layer.select", json!({"layer": b.0, "mode": "toggle"})).unwrap();
        assert_eq!(sel(&s), vec![d]);
        assert_eq!(s.active().unwrap().active_layer, Some(d));
        // The last selected layer can't be toggled off.
        s.execute("layer.select", json!({"layer": d.0, "mode": "toggle"})).unwrap();
        assert_eq!(sel(&s), vec![d]);
        // Range from the anchor (a) to c.
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        s.execute("layer.select", json!({"layer": c.0, "mode": "range"})).unwrap();
        assert_eq!(sel(&s), vec![a, b, c]);
        // A second ⇧-click re-ranges from the same anchor.
        s.execute("layer.select", json!({"layer": b.0, "mode": "range"})).unwrap();
        assert_eq!(sel(&s), vec![a, b]);
        assert!(s.execute("layer.select", json!({"layer": b.0, "mode": "bogus"})).is_err());
        // Selection changes are not edits.
        let history = s.active().unwrap().history.entries().len();
        s.execute("select.allLayers", json!({})).unwrap();
        assert_eq!(sel(&s), vec![a, b, c, d], "Select All Layers skips the locked Background");
        s.execute("select.deselectLayers", json!({})).unwrap();
        assert!(sel(&s).is_empty());
        assert_eq!(s.active().unwrap().history.entries().len(), history);
        assert!(!s.is_enabled("layer.align.topEdges"));
    }

    #[test]
    fn find_layers_by_name() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let b = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        s.execute("layer.renameLayer", json!({"layer": a.0, "name": "Sky photo"})).unwrap();
        s.execute("layer.renameLayer", json!({"layer": b.0, "name": "Photo of sea"})).unwrap();
        assert_eq!(doc(&s).layer(a).unwrap().name, "Sky photo");
        let r = s.execute("select.findLayers", json!({"name": "PHOTO"})).unwrap();
        assert_eq!(r["selected"], json!([a.0, b.0]));
        assert!(s.execute("select.findLayers", json!({"name": "nothing"})).is_err());
        assert!(s.execute("layer.renameLayer", json!({})).is_err());
    }

    #[test]
    fn align_each_edge() {
        for depth in [8, 16, 32] {
            let cases: [(&str, [Rect; 2]); 6] = [
                ("topEdges", [Rect::new(10, 5, 20, 15), Rect::new(40, 5, 50, 25)]),
                ("bottomEdges", [Rect::new(10, 20, 20, 30), Rect::new(40, 10, 50, 30)]),
                // Union 5..30: centre 17.5; half-pixel offsets round towards the top.
                ("verticalCenters", [Rect::new(10, 12, 20, 22), Rect::new(40, 7, 50, 27)]),
                ("leftEdges", [Rect::new(10, 5, 20, 15), Rect::new(10, 10, 50, 30)]),
                ("rightEdges", [Rect::new(40, 5, 50, 15), Rect::new(10, 10, 50, 30)]),
                ("horizontalCenters", [Rect::new(25, 5, 35, 15), Rect::new(10, 10, 50, 30)]),
            ];
            for (kind, want) in cases {
                let mut s = session(depth);
                let a = rect_layer(&mut s, Rect::new(10, 5, 20, 15));
                let b = rect_layer(&mut s, Rect::new(40, 10, 50, 30));
                // For the horizontal cases make b wide so it defines the target bounds.
                if !matches!(kind, "topEdges" | "bottomEdges" | "verticalCenters") {
                    s.edit("wide", |doc, _| {
                        doc.layer_mut(b).unwrap().surface_mut().unwrap().fill_rect(Rect::new(10, 10, 50, 30), &[0.0, 1.0, 0.0, 1.0]);
                        Ok(())
                    })
                    .unwrap();
                }
                select_all(&mut s, &[a, b]);
                s.execute(&format!("layer.align.{kind}"), json!({})).unwrap();
                assert_eq!([bounds(&s, a), bounds(&s, b)], want, "{kind} at {depth}-bit");
                // Pixels moved, not just bounds.
                let r = want[0];
                assert_eq!(doc(&s).layer(a).unwrap().surface().unwrap().pixel(r.x0, r.y0)[..4], [1.0, 0.0, 0.0, 1.0]);
                // One undo restores the original positions.
                s.undo();
                assert_eq!(bounds(&s, a), Rect::new(10, 5, 20, 15));
            }
        }
    }

    #[test]
    fn align_single_layer_to_selection() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            let a = rect_layer(&mut s, Rect::new(10, 10, 20, 20));
            assert!(s.is_enabled("layer.align.leftEdges"), "one layer aligns to the canvas");
            s.execute("select.rect", json!({"x": 50, "y": 40, "width": 30, "height": 30})).unwrap();
            assert!(s.is_enabled("layer.align.leftEdges"));
            s.execute("layer.align.rightEdges", json!({})).unwrap();
            assert_eq!(bounds(&s, a), Rect::new(70, 10, 80, 20));
            s.execute("layer.align.verticalCenters", json!({})).unwrap();
            assert_eq!(bounds(&s, a), Rect::new(70, 50, 80, 60));
            s.execute("layer.align.topEdges", json!({"to": "canvas"})).unwrap();
            assert_eq!(bounds(&s, a), Rect::new(70, 0, 80, 10));
        }
    }

    #[test]
    fn align_single_layer_to_canvas() {
        // Photoshop: a single selected layer with no active selection aligns to the canvas.
        let mut s = session(8); // 100 x 80
        let a = rect_layer(&mut s, Rect::new(10, 10, 20, 20)); // 10 x 10
        assert!(s.is_enabled("layer.align.horizontalCenters"));
        s.execute("layer.align.horizontalCenters", json!({})).unwrap();
        assert_eq!(bounds(&s, a), Rect::new(45, 10, 55, 20), "centred on the 100px-wide canvas");
        s.execute("layer.align.verticalCenters", json!({})).unwrap();
        assert_eq!(bounds(&s, a), Rect::new(45, 35, 55, 45), "centred on the 80px-tall canvas");
        s.execute("layer.align.leftEdges", json!({})).unwrap();
        assert_eq!(bounds(&s, a), Rect::new(0, 35, 10, 45), "left edge to the canvas");
    }

    #[test]
    fn distribute_uneven_layers() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            let a = rect_layer(&mut s, Rect::new(0, 0, 10, 10));
            let b = rect_layer(&mut s, Rect::new(12, 0, 32, 10)); // 20 wide
            let c = rect_layer(&mut s, Rect::new(70, 0, 100, 10)); // 30 wide
            select_all(&mut s, &[a, b, c]);
            assert!(s.is_enabled("layer.distribute.leftEdges"));
            s.execute("layer.distribute.leftEdges", json!({})).unwrap();
            assert_eq!(bounds(&s, b), Rect::new(35, 0, 55, 10), "left edges 0, 35, 70");
            s.undo();
            s.execute("layer.distribute.horizontalCenters", json!({})).unwrap();
            // centres 5 and 85 → middle centre 45
            assert_eq!(bounds(&s, b), Rect::new(35, 0, 55, 10));
            s.undo();
            s.execute("layer.distribute.rightEdges", json!({})).unwrap();
            // right edges 10 and 100 → 55
            assert_eq!(bounds(&s, b), Rect::new(35, 0, 55, 10));
            s.undo();
            s.execute("layer.distribute.horizontally", json!({})).unwrap();
            // span 100, widths 60 → gaps of 20: b at 30..50
            assert_eq!(bounds(&s, b), Rect::new(30, 0, 50, 10));
            assert_eq!(bounds(&s, a), Rect::new(0, 0, 10, 10));
            assert_eq!(bounds(&s, c), Rect::new(70, 0, 100, 10));
            s.undo();
            assert_eq!(bounds(&s, b), Rect::new(12, 0, 32, 10));
        }
        // Vertical: tall layers with uneven heights.
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(0, 0, 10, 10));
        let b = rect_layer(&mut s, Rect::new(0, 50, 10, 54));
        let c = rect_layer(&mut s, Rect::new(0, 60, 10, 80));
        select_all(&mut s, &[a, b, c]);
        s.execute("layer.distribute.vertically", json!({})).unwrap();
        // span 80, heights 34 → gaps 23: b at 33..37
        assert_eq!(bounds(&s, b), Rect::new(0, 33, 10, 37));
        s.undo();
        s.execute("layer.distribute.topEdges", json!({})).unwrap();
        assert_eq!(bounds(&s, b), Rect::new(0, 30, 10, 34));
        s.undo();
        s.execute("layer.distribute.bottomEdges", json!({})).unwrap();
        assert_eq!(bounds(&s, b), Rect::new(0, 41, 10, 45));
        s.undo();
        s.execute("layer.distribute.verticalCenters", json!({})).unwrap();
        // centres 5 and 70 → 37.5, b is 4 tall → top 35.5 → rounds to 36 (35 for ties down)
        let y0 = bounds(&s, b).y0;
        assert!((35..=36).contains(&y0), "{y0}");
        // Two layers can't be distributed.
        select_all(&mut s, &[a, b]);
        assert!(!s.is_enabled("layer.distribute.leftEdges"));
    }

    #[test]
    fn merge_selected_into_top_most() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            let a = rect_layer(&mut s, Rect::new(0, 0, 10, 10));
            let b = rect_layer(&mut s, Rect::new(20, 0, 30, 10));
            let c = rect_layer(&mut s, Rect::new(40, 0, 50, 10));
            s.execute("layer.renameLayer", json!({"layer": c.0, "name": "Top"})).unwrap();
            select_all(&mut s, &[a, c]);
            let before = doc(&s).layer_count();
            let m = LayerId(s.execute("layer.mergeLayers", json!({})).unwrap()["layer"].as_u64().unwrap());
            let d = doc(&s);
            assert_eq!(d.layer_count(), before - 1);
            let ml = d.layer(m).unwrap();
            assert_eq!(ml.name, "Top");
            assert_eq!(layer_bounds(ml).unwrap(), Rect::new(0, 0, 50, 10));
            assert_eq!(ml.surface().unwrap().pixel(5, 5)[..4], [1.0, 0.0, 0.0, 1.0]);
            assert_eq!(ml.surface().unwrap().pixel(25, 5)[3], 0.0, "b was not merged");
            assert!(d.layer(b).is_some() && d.layer(a).is_none());
            assert_eq!(d.layers.last().unwrap().id, m, "merged layer sits where the top-most was");
            assert_eq!(sel(&s), vec![m]);
            s.undo();
            assert_eq!(doc(&s).layer_count(), before);
            assert_eq!(sel(&s), vec![c], "undo leaves a valid selection");
            // One layer selected: Merge Down.
            s.execute("layer.mergeLayers", json!({})).unwrap();
            assert_eq!(doc(&s).layer_count(), before - 1);
        }
    }

    #[test]
    fn group_from_layers_preserves_order() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let b = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let c = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let d = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        select_all(&mut s, &[c, a]);
        let g = LayerId(s.execute("layer.new.groupFromLayers", json!({"name": "Pair"})).unwrap()["layer"].as_u64().unwrap());
        let docu = doc(&s);
        let gl = docu.layer(g).unwrap();
        assert_eq!(gl.name, "Pair");
        assert_eq!(gl.children().unwrap().iter().map(|l| l.id).collect::<Vec<_>>(), vec![a, c]);
        // Root (bottom-to-top): Background, b, Pair (where c was), d.
        assert_eq!(docu.layers.iter().map(|l| l.id).skip(1).collect::<Vec<_>>(), vec![b, g, d]);
        assert_eq!(sel(&s), vec![g]);
        s.undo();
        assert_eq!(doc(&s).layers.len(), 5);
        // ⌘G with several selected groups them too.
        select_all(&mut s, &[b, d]);
        s.execute("layer.groupLayers", json!({})).unwrap();
        assert_eq!(doc(&s).layers.len(), 4);
    }

    #[test]
    fn linked_layers_move_together_and_undo() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            let a = rect_layer(&mut s, Rect::new(0, 0, 10, 10));
            let b = rect_layer(&mut s, Rect::new(20, 20, 30, 30));
            let c = rect_layer(&mut s, Rect::new(50, 50, 60, 60));
            assert!(!s.is_enabled("layer.linkLayers"));
            select_all(&mut s, &[a, b]);
            assert_eq!(s.execute("layer.linkLayers", json!({})).unwrap()["linked"], true);
            let g = doc(&s).layer(a).unwrap().link_group;
            assert!(g.is_some() && doc(&s).layer(b).unwrap().link_group == g);
            // Move only a: b follows, c stays.
            s.execute("layer.select", json!({"layer": a.0})).unwrap();
            s.execute("layer.translate", json!({"dx": 5, "dy": 3})).unwrap();
            assert_eq!(bounds(&s, a), Rect::new(5, 3, 15, 13));
            assert_eq!(bounds(&s, b), Rect::new(25, 23, 35, 33));
            assert_eq!(bounds(&s, c), Rect::new(50, 50, 60, 60));
            s.undo();
            assert_eq!(bounds(&s, a), Rect::new(0, 0, 10, 10));
            assert_eq!(bounds(&s, b), Rect::new(20, 20, 30, 30));
            // Select Linked Layers from just a.
            s.execute("layer.selectLinkedLayers", json!({})).unwrap();
            assert_eq!(sel(&s), vec![a, b]);
            // Multi-selection translate moves all selected (c added, unlinked).
            s.execute("layer.select", json!({"layer": c.0, "mode": "toggle"})).unwrap();
            s.execute("layer.translate", json!({"dx": -1, "dy": 0})).unwrap();
            assert_eq!(bounds(&s, c), Rect::new(49, 50, 59, 60));
            assert_eq!(bounds(&s, a), Rect::new(-1, 0, 9, 10));
            // Toggle: linking the same group again unlinks.
            select_all(&mut s, &[a, b]);
            assert_eq!(s.execute("layer.linkLayers", json!({})).unwrap()["linked"], false);
            assert!(doc(&s).layer(a).unwrap().link_group.is_none());
        }
    }

    #[test]
    fn selection_pruned_after_delete_and_undo() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let b = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let c = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        select_all(&mut s, &[a, c]);
        // Deleting one layer explicitly prunes it from the set.
        s.execute("layer.delete", json!({"layer": a.0})).unwrap();
        assert_eq!(sel(&s), vec![c]);
        s.undo();
        assert!(sel(&s).iter().all(|id| doc(&s).layer(*id).is_some()));
        // Deleting with several selected removes them all, then selects the layer below.
        select_all(&mut s, &[a, c]);
        s.execute("layer.delete", json!({})).unwrap();
        assert!(doc(&s).layer(a).is_none() && doc(&s).layer(c).is_none());
        assert_eq!(doc(&s).layer_count(), 2);
        assert_eq!(sel(&s).len(), 1);
        assert!(s.active().unwrap().selected_layers.iter().all(|id| doc(&s).layer(*id).is_some()));
        s.undo();
        assert_eq!(doc(&s).layer_count(), 4);
        let _ = b;
        // Redo then undo again keeps the set valid.
        s.redo();
        assert!(sel(&s).iter().all(|id| doc(&s).layer(*id).is_some()));
    }

    #[test]
    fn duplicate_hide_lock_and_reverse_selected() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let b = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let c = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        select_all(&mut s, &[a, c]);
        let r = s.execute("layer.duplicate", json!({})).unwrap();
        assert_eq!(r["layers"].as_array().unwrap().len(), 2);
        assert_eq!(doc(&s).layer_count(), 6);
        assert_eq!(sel(&s).len(), 2);
        assert!(sel(&s).iter().all(|id| *id != a && *id != c));
        s.undo();
        select_all(&mut s, &[a, b]);
        s.execute("layer.hideLayers", json!({})).unwrap();
        assert!(!doc(&s).layer(a).unwrap().visible && !doc(&s).layer(b).unwrap().visible && doc(&s).layer(c).unwrap().visible);
        s.execute("layer.showLayers", json!({})).unwrap();
        assert!(doc(&s).layer(a).unwrap().visible);
        s.execute("layer.lockLayers", json!({})).unwrap();
        assert!(doc(&s).layer(a).unwrap().locks.all && doc(&s).layer(b).unwrap().locks.all);
        assert!(s.execute("layer.translate", json!({"dx": 1})).is_err(), "locked layers don't move");
        s.execute("layer.lockLayers", json!({})).unwrap();
        assert!(!doc(&s).layer(a).unwrap().locks.all);
        s.execute("layer.lockLayers", json!({"position": true})).unwrap();
        assert!(doc(&s).layer(b).unwrap().locks.position && !doc(&s).layer(c).unwrap().locks.position);
        // Reverse a, b, c.
        select_all(&mut s, &[a, b, c]);
        s.execute("layer.arrange.reverse", json!({})).unwrap();
        assert_eq!(doc(&s).layers.iter().skip(1).map(|l| l.id).collect::<Vec<_>>(), vec![c, b, a]);
        assert_eq!(s.active().unwrap().history.entries().last().map(|e| e.as_str()), Some("Reverse"));
    }

    #[test]
    fn inspect_reports_selection() {
        let mut s = session(8);
        let a = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        let b = rect_layer(&mut s, Rect::new(0, 0, 5, 5));
        select_all(&mut s, &[a, b]);
        let v = s.execute("document.inspect", json!({})).unwrap();
        assert_eq!(v["selectedLayers"], json!([a.0, b.0]));
        assert_eq!(v["layers"][0]["selected"], true);
    }
}
