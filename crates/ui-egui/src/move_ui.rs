//! Move tool drags shown live (#128): while the pointer drags, the canvas shows the document with
//! the moving layers already at the pointer, exactly as `layer.translate` will leave them, and
//! recomposites only where they were and where they are. Releasing commits one `layer.translate`
//! (one history step) whose damage rect refreshes the same area.

use std::sync::Arc;

use photocraft_doc::{DocId, Document, LayerId};
use photocraft_geom::Rect;

use crate::PhotocraftApp;
use crate::state::Tool;

/// Preview keys of move drags: `BASE + n`, one per offset shown (see `canvas::display_doc`).
const BASE: u64 = 1 << 39;

pub(crate) struct MovePreview {
    doc: DocId,
    revision: u64,
    /// Layers that move ([`photocraft_engine::layer_multi_cmds::move_targets`]).
    ids: Vec<LayerId>,
    /// What moving them can change at offset (0, 0) (`None` = anything).
    bounds: Option<Rect>,
    /// Offsets shown so far, by preview key (`BASE + index`).
    offsets: Vec<(i32, i32)>,
    /// The document at the latest offset.
    shown: Option<Arc<Document>>,
}

impl MovePreview {
    fn key(&self) -> u64 {
        BASE + self.offsets.len() as u64
    }
    fn offset_of(&self, key: u64) -> Option<(i32, i32)> {
        if key == 0 {
            return Some((0, 0));
        }
        let i = usize::try_from(key.checked_sub(BASE + 1)?).ok()?;
        self.offsets.get(i).copied()
    }
    fn area(&self, d: (i32, i32)) -> Option<Rect> {
        let b = self.bounds?;
        Some(if b.is_empty() { b } else { b.translate(d.0, d.1).inflate(1) })
    }
}

/// The whole-pixel offset of the current Move drag on document `idx`, if one is under way.
fn drag_offset(app: &PhotocraftApp) -> Option<(i32, i32)> {
    let d = app.drag.as_ref().filter(|d| d.tool == Tool::Move)?;
    let end = d.points.last().map_or(d.start, |p| [p[0], p[1]]);
    let dx = (end[0] - d.start[0]).round().clamp(-1e7, 1e7) as i32;
    let dy = (end[1] - d.start[1]).round().clamp(-1e7, 1e7) as i32;
    Some((dx, dy))
}

/// The document to show while a Move drag is under way on document `idx`: the moving layers at
/// the pointer. `None` without a drag (or when the layers can't move: locked, say; the drag then
/// shows its arrow and the release reports why).
pub(crate) fn display_doc(app: &mut PhotocraftApp, idx: usize) -> Option<(Arc<Document>, u64)> {
    if app.session.active_index() != Some(idx) {
        return None;
    }
    let Some(offset) = drag_offset(app) else {
        app.move_preview = None;
        return None;
    };
    let st = app.session.documents().get(idx)?;
    let (doc_id, revision, doc) = (st.doc.id, st.revision, st.doc.clone());
    let fresh = app.move_preview.as_ref().is_some_and(|p| p.doc == doc_id && p.revision == revision);
    if !fresh {
        let ids = photocraft_engine::layer_multi_cmds::move_targets(&doc, &st.selected_layers());
        if ids.is_empty() {
            return None;
        }
        let bounds = photocraft_engine::layer_multi_cmds::layers_damage(&doc, &doc, &ids);
        app.move_preview = Some(MovePreview { doc: doc_id, revision, ids, bounds, offsets: Vec::new(), shown: None });
    }
    let p = app.move_preview.as_mut()?;
    if offset == (0, 0) && p.offsets.is_empty() {
        return None;
    }
    if p.offsets.last() != Some(&offset) {
        let t0 = crate::gpu_canvas::now_ms();
        match photocraft_engine::layer_multi_cmds::moved(&doc, &p.ids, offset.0, offset.1) {
            Ok(d) => {
                // Duotone documents display through their inks.
                let d = photocraft_engine::mode_cmds::display_document(&d).unwrap_or(d);
                p.shown = Some(Arc::new(d));
                p.offsets.push(offset);
            }
            Err(_) => {
                p.shown = None;
                return None;
            }
        }
        app.perf.span("move preview", crate::gpu_canvas::now_ms() - t0);
    }
    let p = app.move_preview.as_ref()?;
    Some((p.shown.clone()?, p.key()))
}

/// What changed between preview (or document) key `seen` and `now` of the current Move drag on
/// document `doc` at `revision`: where the moving layers were in both. `None` when either key
/// isn't one of this drag's (the canvas then recomposites everything).
pub(crate) fn damage(app: &PhotocraftApp, doc: DocId, revision: u64, seen: u64, now: u64) -> Option<Rect> {
    let p = app.move_preview.as_ref().filter(|p| p.doc == doc && p.revision == revision)?;
    if seen == now || (seen < BASE && seen != 0) || (now < BASE && now != 0) {
        return None;
    }
    let (a, b) = (p.area(p.offset_of(seen)?)?, p.area(p.offset_of(now)?)?);
    Some(if a.is_empty() {
        b
    } else if b.is_empty() {
        a
    } else {
        a.union(&b)
    })
}

/// Whether the canvas shows a Move drag's layers at the pointer.
pub(crate) fn showing(app: &PhotocraftApp) -> bool {
    app.move_preview.as_ref().is_some_and(|p| p.shown.is_some())
}

/// Whether `key` is a Move drag preview key.
pub(crate) fn is_preview_key(key: u64) -> bool {
    key > BASE && key < BASE << 1
}

/// Release: one `layer.translate` by the drag's offset. The canvas already shows the result, so
/// its caches are told it showed the document: the command's damage rect refreshes that area.
pub(crate) fn finish(app: &mut PhotocraftApp, dx: f64, dy: f64) {
    // Only when the canvas shows this very offset (else it recomposites everything once).
    let at = (dx.clamp(-1e7, 1e7) as i32, dy.clamp(-1e7, 1e7) as i32);
    let shown = app.move_preview.take().filter(|p| p.shown.is_some() && p.offsets.last() == Some(&at));
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    if app.run("layer.translate", serde_json::json!({"dx": dx, "dy": dy})).is_err() {
        return;
    }
    let Some(p) = shown else { return };
    crate::canvas::shown_as_document(app, p.doc, is_preview_key);
}
