//! Undo/redo history.
//!
//! Because pixel tiles are `Arc`-shared copy-on-write (see `photocraft-raster`), a full
//! [`Document`] clone costs O(layers + tiles) pointer copies, not pixel copies. History therefore
//! stores whole-document snapshots per transaction, which is simple, obviously correct, and the
//! same approach Photoshop's History panel exposes to users (one state per step).
//!
//! Memory is bounded by `max_states` plus an approximate byte budget. Only tiles *not shared*
//! with the current document count against it.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use photocraft_doc::Document;

#[derive(Clone, Debug)]
pub struct HistoryState {
    pub label: String,
    pub doc: Arc<Document>,
}

#[derive(Clone, Debug)]
pub struct History {
    /// Past states; the last one is the state *before* the current document.
    undo: VecDeque<HistoryState>,
    redo: Vec<HistoryState>,
    pub max_states: usize,
    /// Pixel memory budget in bytes for the current document plus the tiles only history holds
    /// (0 = unlimited). [`History::trim`] drops the oldest states beyond it.
    pub max_bytes: usize,
    /// Label of the step that produced the current document.
    current_label: String,
}

impl Default for History {
    fn default() -> Self {
        Self::new(50)
    }
}

impl History {
    pub fn new(max_states: usize) -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new(), max_states: max_states.max(1), max_bytes: 0, current_label: "Open".into() }
    }

    /// Record that `before` was replaced by a new current document via step `label`.
    pub fn record(&mut self, label: impl Into<String>, before: Arc<Document>) {
        let prev_label = std::mem::replace(&mut self.current_label, label.into());
        self.undo.push_back(HistoryState { label: prev_label, doc: before });
        self.redo.clear();
        while self.undo.len() > self.max_states {
            self.undo.pop_front();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.can_undo().then_some(self.current_label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Undo: returns the document to make current.
    pub fn undo(&mut self, current: Arc<Document>) -> Option<Arc<Document>> {
        let prev = self.undo.pop_back()?;
        let label = std::mem::replace(&mut self.current_label, prev.label);
        self.redo.push(HistoryState { label, doc: current });
        Some(prev.doc)
    }

    pub fn redo(&mut self, current: Arc<Document>) -> Option<Arc<Document>> {
        let next = self.redo.pop()?;
        let label = std::mem::replace(&mut self.current_label, next.label);
        self.undo.push_back(HistoryState { label, doc: current });
        Some(next.doc)
    }

    /// Entries for a History panel: past labels oldest→newest, then the current label.
    pub fn entries(&self) -> Vec<String> {
        self.undo.iter().map(|s| s.label.clone()).chain(std::iter::once(self.current_label.clone())).collect()
    }

    /// Document of past entry `i`, indexed like [`History::entries`] (0 = oldest). The last entry is
    /// the current document, which the history does not hold, so it (and any index past it) is `None`.
    pub fn state(&self, i: usize) -> Option<Arc<Document>> {
        self.undo.get(i).map(|s| s.doc.clone())
    }

    /// Number of past states (entries before the current one).
    pub fn past_len(&self) -> usize {
        self.undo.len()
    }

    /// Rename the current step (e.g. after folding several steps into one with [`Self::purge_last`]).
    pub fn set_current_label(&mut self, label: impl Into<String>) {
        self.current_label = label.into();
    }

    /// Forget the most recent undo state and every redo state (Edit › Purge › Undo): the
    /// last step can no longer be undone and its pixels are released.
    pub fn purge_last(&mut self) -> bool {
        self.redo.clear();
        self.undo.pop_back().is_some()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Approximate unique pixel bytes held by history (tiles not shared with `current`).
    pub fn unique_bytes(&self, current: &Document) -> usize {
        let mut seen = HashSet::new();
        tile_bytes(current, &mut seen);
        self.undo.iter().chain(self.redo.iter()).map(|s| tile_bytes(&s.doc, &mut seen)).sum()
    }

    /// Pixel bytes of `current` plus the tiles only history holds (what [`History::trim`]
    /// bounds by [`History::max_bytes`]).
    pub fn pixel_bytes(&self, current: &Document) -> usize {
        let mut seen = HashSet::new();
        let own = tile_bytes(current, &mut seen);
        self.undo.iter().chain(self.redo.iter()).fold(own, |n, s| n.saturating_add(tile_bytes(&s.doc, &mut seen)))
    }

    /// Keep pixel memory within [`History::max_bytes`]: the current document's tiles plus the
    /// tiles only history holds (newest states first). The oldest undo states that don't fit are
    /// dropped; the most recent one is always kept so the last step can be undone. Returns how
    /// many states were dropped.
    pub fn trim(&mut self, current: &Document) -> usize {
        if self.max_bytes == 0 || self.undo.len() <= 1 {
            return 0;
        }
        let mut seen = HashSet::new();
        let mut total = tile_bytes(current, &mut seen);
        for s in self.redo.iter().rev() {
            total = total.saturating_add(tile_bytes(&s.doc, &mut seen));
        }
        let mut keep = 0;
        for s in self.undo.iter().rev() {
            total = total.saturating_add(tile_bytes(&s.doc, &mut seen));
            if total > self.max_bytes && keep >= 1 {
                break;
            }
            keep += 1;
        }
        let drop = self.undo.len() - keep;
        self.undo.drain(..drop);
        drop
    }
}

/// Bytes of the pixel tiles of `doc` (layers, masks, alpha channels) not already in `seen`.
fn tile_bytes(doc: &Document, seen: &mut HashSet<usize>) -> usize {
    let mut add = |s: &photocraft_doc::Surface| s.tiles().filter(|(_, t)| seen.insert(Arc::as_ptr(t) as usize)).map(|(_, t)| t.bytes().len()).sum::<usize>();
    let mut n = 0;
    for (_, _, l) in doc.walk() {
        if let Some(s) = l.surface() {
            n += add(s);
        }
        if let Some(m) = &l.mask {
            n += add(&m.surface);
        }
    }
    for c in doc.channels.iter().chain(&doc.quick_mask) {
        n += add(&c.surface);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{Color, ColorMode, Layer, SampleType, Size};

    fn base() -> Document {
        Document::with_background("h", Size::new(64, 64), ColorMode::Rgb, SampleType::U8, Color::WHITE)
    }

    /// Apply an edit the way the engine does: snapshot, mutate a clone, record.
    fn edit(h: &mut History, cur: &mut Arc<Document>, label: &str, f: impl FnOnce(&mut Document)) {
        let before = cur.clone();
        let mut d = (**cur).clone();
        f(&mut d);
        *cur = Arc::new(d);
        h.record(label, before);
    }

    #[test]
    fn undo_redo_cycle() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        edit(&mut h, &mut cur, "New Layer", |d| {
            d.insert_above(None, Layer::raster("L", d.pixel_format()));
        });
        assert_eq!(cur.layers.len(), 2);
        assert_eq!(h.undo_label(), Some("New Layer"));
        cur = h.undo(cur).unwrap();
        assert_eq!(cur.layers.len(), 1);
        assert_eq!(h.redo_label(), Some("New Layer"));
        cur = h.redo(cur).unwrap();
        assert_eq!(cur.layers.len(), 2);
        assert!(h.redo(cur.clone()).is_none());
    }

    #[test]
    fn state_accessor_matches_entries() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        let open = cur.clone();
        edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
        edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
        assert_eq!(h.entries().len(), h.past_len() + 1);
        assert!(Arc::ptr_eq(&h.state(0).unwrap(), &open));
        assert_eq!(h.state(1).unwrap().name, "a");
        assert!(h.state(2).is_none(), "the current document is not held");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
        cur = h.undo(cur).unwrap();
        edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
        assert!(!h.can_redo());
        assert_eq!(h.entries(), vec!["Open".to_string(), "B".to_string()]);
    }

    #[test]
    fn max_states_bound() {
        let mut h = History::new(3);
        let mut cur = Arc::new(base());
        for i in 0..10 {
            edit(&mut h, &mut cur, &format!("step {i}"), |d| d.name = format!("{i}"));
        }
        let mut n = 0;
        while let Some(d) = h.undo(cur.clone()) {
            cur = d;
            n += 1;
        }
        assert_eq!(n, 3);
        assert_eq!(cur.name, "6");
    }

    #[test]
    fn byte_budget_drops_oldest_states_but_keeps_one() {
        let mut h = History::new(50);
        let mut cur = Arc::new(base());
        let tile = 256 * 256 * 4;
        // Each step repaints the background's tile: one unique tile per state.
        for i in 0..6 {
            edit(&mut h, &mut cur, &format!("paint {i}"), |d| {
                let id = d.layers[0].id;
                d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
            });
        }
        assert_eq!(h.trim(&cur), 0, "unlimited by default");
        assert_eq!(h.past_len(), 6);
        // The current tile plus three history tiles fit.
        h.max_bytes = 4 * tile;
        assert_eq!(h.trim(&cur), 3);
        assert_eq!(h.past_len(), 3);
        assert_eq!(h.unique_bytes(&cur), 3 * tile);
        assert_eq!(h.pixel_bytes(&cur), 4 * tile, "the current tile plus history's");
        assert_eq!(h.entries()[0], "paint 2");
        // A budget smaller than one state still keeps the last step undoable.
        h.max_bytes = 1;
        h.trim(&cur);
        assert_eq!(h.past_len(), 1);
        assert!(h.undo(cur.clone()).is_some());
    }

    #[test]
    fn history_shares_untouched_tiles() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        assert_eq!(h.unique_bytes(&cur), 0);
        // rename only: no pixel changes, history holds no unique tiles
        edit(&mut h, &mut cur, "Rename", |d| d.name = "x".into());
        assert_eq!(h.unique_bytes(&cur), 0);
        // paint one pixel: exactly one tile becomes unique to history
        edit(&mut h, &mut cur, "Paint", |d| {
            let id = d.layers[0].id;
            d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[0.0, 0.0, 0.0, 1.0]);
        });
        assert_eq!(h.unique_bytes(&cur), 256 * 256 * 4);
    }
}
