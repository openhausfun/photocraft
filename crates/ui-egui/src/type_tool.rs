//! Type tool: click to add point text, drag to add paragraph text, edit inline with a caret.
//!
//! Every edit is an engine `type.*` command carrying the session's `coalesce` key, so a whole
//! typing session is one "Edit Type" history step and automation sees exactly what the user does.
//! Offsets in [`TextEdit`] are character indices (the engine's unit); the layout works in bytes.

use std::sync::Arc;

use egui::{Color32, Pos2, Stroke};
use photocraft_doc::{Document, LayerContent, LayerId, TextLayer};
use photocraft_geom::{Affine, Point};
use photocraft_text::TextLayout;
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::TextEdit;

pub const PLACEHOLDER: &str = "Lorem Ipsum";

fn text_layer(doc: &Document, id: LayerId) -> Option<&TextLayer> {
    match &doc.layer(id)?.content {
        LayerContent::Text(t) => Some(t),
        _ => None,
    }
}

fn byte_of(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b)
}

fn char_of(text: &str, bi: usize) -> usize {
    text[..bi.min(text.len())].chars().count()
}

/// Layout of a type layer (cached per document revision) and its text → document transform.
pub fn layout(app: &mut PhotocraftApp, id: LayerId) -> Option<(Arc<TextLayout>, Affine, String)> {
    let st = app.session.active()?;
    let (doc, rev) = (st.doc.clone(), st.revision);
    let t = text_layer(&doc, id)?;
    let key = (doc.id.0, rev, id.0);
    if let Some((k, l)) = &app.type_layout
        && *k == key
    {
        return Some((l.clone(), t.transform, t.text.clone()));
    }
    let l = Arc::new(photocraft_text::shared().lock().ok()?.layout(t, doc.resolution_dpi));
    app.type_layout = Some((key, l.clone()));
    Some((l, t.transform, t.text.clone()))
}

fn to_text(aff: &Affine, x: f64, y: f64) -> (f32, f32) {
    let p = aff.inverse().unwrap_or(Affine::IDENTITY).apply(Point::new(x, y));
    (p.x as f32, p.y as f32)
}

/// Topmost visible type layer whose laid-out text contains the document point.
fn hit_layer(app: &mut PhotocraftApp, x: f64, y: f64) -> Option<LayerId> {
    let doc = app.session.active()?.doc.clone();
    let slop = 6.0 / app.current_zoom().max(0.01);
    let mut ids: Vec<LayerId> =
        doc.walk().into_iter().filter(|(_, _, l)| l.visible && matches!(l.content, LayerContent::Text(_))).map(|(_, _, l)| l.id).collect();
    ids.reverse(); // walk() is bottom-up; hit the topmost first
    ids.into_iter().find(|id| {
        // The pixels shown count too: a PSD's type keeps Photoshop's rendering until edited,
        // which can sit off our layout when fonts are substituted (#123).
        let shown = text_layer(&doc, *id).and_then(|t| t.cache.as_ref()).map(|c| c.content_bounds()).is_some_and(|r| {
            let s = f64::from(slop);
            !r.is_empty() && x >= f64::from(r.x0) - s && x <= f64::from(r.x1) + s && y >= f64::from(r.y0) - s && y <= f64::from(r.y1) + s
        });
        let Some((l, aff, _)) = layout(app, *id) else { return shown };
        let (tx, ty) = to_text(&aff, x, y);
        shown || l.bounds().is_some_and(|b| tx >= b[0] - slop && tx <= b[2] + slop && ty >= b[1] - slop && ty <= b[3] + slop)
    })
}

/// True when the layer's pixels are what our engine draws for it. A PSD's type layer keeps
/// Photoshop's pixels until it is edited, and with substituted fonts or a different line layout
/// they sit somewhere else than the glyphs the caret and selection are placed on.
fn shows_own_layout(doc: &Document, t: &TextLayer) -> bool {
    let Some(cache) = &t.cache else { return false };
    let Ok(mut eng) = photocraft_text::shared().lock() else { return true };
    let ours = eng.render(t, doc.resolution_dpi, doc.pixel_format()).1.surface.content_bounds();
    let have = cache.content_bounds();
    [(ours.x0, have.x0), (ours.y0, have.y0), (ours.x1, have.x1), (ours.y1, have.y1)].iter().all(|(a, b)| (a - b).abs() <= 1)
}

/// Start editing an existing type layer. Like Photoshop, editing shows the text as the type
/// engine lays it out, so a PSD layer is re-rendered first (inside the edit session's history
/// step, so Cancel brings Photoshop's pixels back).
fn begin_edit(app: &mut PhotocraftApp, id: LayerId, key: &str) {
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.clone();
    if let Some(t) = text_layer(&doc, id)
        && !shows_own_layout(&doc, t)
    {
        let _ = app.run("type.edit", json!({"layer": id.0, "coalesce": key}));
    }
}

fn hit_offset(app: &mut PhotocraftApp, id: LayerId, x: f64, y: f64) -> usize {
    let Some((l, aff, text)) = layout(app, id) else { return 0 };
    let (tx, ty) = to_text(&aff, x, y);
    char_of(&text, l.hit_test(tx, ty))
}

fn hex(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// Pointer down with the Type tool. Returns true when the press was consumed (no box drag).
pub fn pointer_down(app: &mut PhotocraftApp, x: f64, y: f64, shift: bool) -> bool {
    if let Some(ed) = app.ui.text_edit.clone() {
        let id = LayerId(ed.layer);
        if hit_layer(app, x, y) == Some(id) {
            let off = hit_offset(app, id, x, y);
            if let Some(e) = app.ui.text_edit.as_mut() {
                e.caret = off;
                if !shift {
                    e.anchor = off;
                }
                e.dragging = true;
            }
            return true;
        }
        commit(app);
        return true; // Photoshop: a click outside commits without starting new text
    }
    if let Some(id) = hit_layer(app, x, y) {
        let _ = app.session.select_layer(id);
        let key = session_key(app);
        begin_edit(app, id, &key);
        let off = hit_offset(app, id, x, y);
        app.ui.text_edit = Some(TextEdit { layer: id.0, caret: off, anchor: off, session: key, created: false, dragging: true, preedit: None });
        return true;
    }
    false
}

pub fn pointer_move(app: &mut PhotocraftApp, x: f64, y: f64) {
    let Some(ed) = app.ui.text_edit.clone() else { return };
    if ed.dragging {
        let off = hit_offset(app, LayerId(ed.layer), x, y);
        if let Some(e) = app.ui.text_edit.as_mut() {
            e.caret = off;
        }
    }
}

/// Pointer up. `rect` is the dragged box (document px) when no edit session consumed the press.
pub fn pointer_up(app: &mut PhotocraftApp, start: [f64; 2], end: [f64; 2]) {
    if let Some(e) = app.ui.text_edit.as_mut() {
        e.dragging = false;
        return;
    }
    let (w, h) = ((end[0] - start[0]).abs(), (end[1] - start[1]).abs());
    let min = 4.0 / app.current_zoom().max(0.01) as f64;
    let o = app.ui.tool_options.clone();
    let mut p = json!({
        "text": PLACEHOLDER,
        "font": o.type_font,
        "fontStyle": o.type_style,
        "size": o.type_size,
        "align": o.type_align,
        "color": hex(app.session.tools.foreground),
    });
    if w >= min && h >= min {
        p["box"] = json!([start[0].min(end[0]).round(), start[1].min(end[1]).round(), w.round(), h.round()]);
    } else {
        p["x"] = json!(start[0].round());
        p["y"] = json!(start[1].round());
    }
    let key = session_key(app);
    p["coalesce"] = json!(key);
    if let Ok(v) = app.run("type.create", p)
        && let Some(id) = v.get("layer").and_then(serde_json::Value::as_u64)
    {
        if o.type_aa != "sharp" {
            let _ = app.run("type.edit", json!({"layer": id, "antialias": o.type_aa, "coalesce": key}));
        }
        // Like Photoshop: the placeholder is selected, so typing replaces it.
        let n = PLACEHOLDER.chars().count();
        app.ui.text_edit = Some(TextEdit { layer: id, caret: n, anchor: 0, session: key, created: true, dragging: false, preedit: None });
    }
}

fn session_key(app: &mut PhotocraftApp) -> String {
    format!("type-{}", app.ui.alloc_id())
}

fn current_text(app: &PhotocraftApp, id: LayerId) -> Option<String> {
    Some(text_layer(&app.session.active()?.doc, id)?.text.clone())
}

/// Replace the selection with `s`.
fn insert(app: &mut PhotocraftApp, s: &str) {
    let Some(ed) = app.ui.text_edit.clone() else { return };
    let (a, b) = (ed.caret.min(ed.anchor), ed.caret.max(ed.anchor));
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    if a == b && s.is_empty() {
        return;
    }
    if app.run("type.edit", json!({"layer": ed.layer, "replace": {"start": a, "end": b, "text": s}, "coalesce": ed.session})).is_ok()
        && let Some(e) = app.ui.text_edit.as_mut()
    {
        e.caret = a + s.chars().count();
        e.anchor = e.caret;
    }
}

/// Alt+←/→: kern the pair before the caret by `by` (1/1000 em). No pair (caret at a text or line
/// edge): nothing happens, like Photoshop. Not coalesced: one history step per press.
fn kern_pair(app: &mut PhotocraftApp, id: LayerId, caret: usize, by: f32) {
    let Some(text) = current_text(app, id) else { return };
    let before = caret.checked_sub(1).and_then(|i| text.chars().nth(i));
    let after = text.chars().nth(caret);
    if before.is_none_or(|c| c == '\n') || after.is_none_or(|c| c == '\n') {
        return;
    }
    let _ = app.run("type.edit", json!({"layer": id.0, "kernPair": {"at": caret, "by": by}}));
}

/// IME composition. The preedit text is written into the layer (so it lays out and reflows like
/// typed text) and replaced by every update; `commit` makes the result final.
fn ime_update(app: &mut PhotocraftApp, s: &str, commit: bool) {
    let Some(ed) = app.ui.text_edit.clone() else { return };
    let Some(text) = current_text(app, LayerId(ed.layer)) else { return };
    let n = text.chars().count();
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let (start, end) = match ed.preedit {
        Some((p, l)) if p.saturating_add(l) <= n => (p, p + l),
        _ => (ed.caret.min(ed.anchor).min(n), ed.caret.max(ed.anchor).min(n)),
    };
    if start == end && s.is_empty() {
        if let Some(e) = app.ui.text_edit.as_mut() {
            e.preedit = None;
        }
        return;
    }
    let len = s.chars().count();
    if app.run("type.edit", json!({"layer": ed.layer, "replace": {"start": start, "end": end, "text": s}, "coalesce": ed.session})).is_ok()
        && let Some(e) = app.ui.text_edit.as_mut()
    {
        e.caret = start + len;
        e.anchor = e.caret;
        e.preedit = if commit || len == 0 { None } else { Some((start, len)) };
    }
}

/// Character index of the previous / next word boundary.
fn word_boundary(text: &str, from: usize, forward: bool) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut i = from.min(chars.len());
    if forward {
        while i < chars.len() && !chars[i].is_alphanumeric() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_alphanumeric() {
            i += 1;
        }
    } else {
        while i > 0 && !chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
    }
    i
}

/// Double-click: select the word under the caret.
pub fn select_word(app: &mut PhotocraftApp) {
    let Some(ed) = app.ui.text_edit.clone() else { return };
    let Some(text) = current_text(app, LayerId(ed.layer)) else { return };
    let chars: Vec<char> = text.chars().collect();
    let (mut a, mut b) = (ed.caret.min(chars.len()), ed.caret.min(chars.len()));
    while a > 0 && chars[a - 1].is_alphanumeric() {
        a -= 1;
    }
    while b < chars.len() && chars[b].is_alphanumeric() {
        b += 1;
    }
    if let Some(e) = app.ui.text_edit.as_mut() {
        (e.anchor, e.caret) = (a, b);
    }
}

/// Caret on the neighbouring line (±1; a column in vertical type), keeping the position
/// along the line. Works in line space, so it serves both orientations.
fn line_step(app: &mut PhotocraftApp, id: LayerId, caret: usize, dir: i32) -> usize {
    let Some((l, _, text)) = layout(app, id) else { return caret };
    let (x, top, bottom) = l.caret(byte_of(&text, caret));
    let h = (bottom - top).max(1.0);
    let y = if dir < 0 { top - h * 0.5 } else { bottom + h * 0.5 };
    let Some(b) = l.line_bounds() else { return caret };
    if y < b[1] {
        return 0;
    }
    if y > b[3] {
        return text.chars().count();
    }
    char_of(&text, l.hit_test_line(x, y))
}

fn is_vertical(app: &PhotocraftApp, id: LayerId) -> bool {
    app.session.active().and_then(|s| text_layer(&s.doc, id)).is_some_and(|t| t.orientation == photocraft_doc::text::Orientation::Vertical)
}

/// Arrow keys follow the text flow: in vertical type ↑/↓ move along the column (previous/next
/// character) and ←/→ move to the next/previous column (columns advance right to left). Returns
/// the horizontal-type key with the same meaning.
pub(crate) fn flow_key(key: egui::Key, vertical: bool) -> egui::Key {
    use egui::Key;
    if !vertical {
        return key;
    }
    match key {
        Key::ArrowUp => Key::ArrowLeft,
        Key::ArrowDown => Key::ArrowRight,
        Key::ArrowLeft => Key::ArrowDown,
        Key::ArrowRight => Key::ArrowUp,
        k => k,
    }
}

/// Line start / end for the caret's line.
fn line_edge(app: &mut PhotocraftApp, id: LayerId, caret: usize, end: bool) -> usize {
    let Some((l, _, text)) = layout(app, id) else { return caret };
    let b = byte_of(&text, caret);
    let line = l.lines.iter().find(|ln| b >= ln.range.start && b <= ln.range.end).or(l.lines.last());
    line.map_or(caret, |ln| char_of(&text, if end { ln.range.end } else { ln.range.start }))
}

/// Keyboard input while editing. Returns true when a type edit session is active (single-key
/// tool shortcuts must then be skipped). Handled events are removed from the frame's input.
pub fn handle_keys(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    let Some(ed) = app.ui.text_edit.clone() else { return false };
    let id = LayerId(ed.layer);
    let Some(text) = current_text(app, id) else {
        app.ui.text_edit = None;
        return false;
    };
    // Undo/redo can shorten the text under us.
    let n = text.chars().count();
    if let Some(e) = app.ui.text_edit.as_mut() {
        e.caret = e.caret.min(n);
        e.anchor = e.anchor.min(n);
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(530)); // caret blink
    let events = ctx.input(|i| i.events.clone());
    let vertical_flow = is_vertical(app, id);
    let mut handled = vec![false; events.len()];
    for (k, ev) in events.iter().enumerate() {
        let Some(ed) = app.ui.text_edit.clone() else { break };
        let (a, b) = (ed.caret.min(ed.anchor), ed.caret.max(ed.anchor));
        let text = current_text(app, id).unwrap_or_default();
        let n = text.chars().count();
        let set = |app: &mut PhotocraftApp, caret: usize, extend: bool| {
            if let Some(e) = app.ui.text_edit.as_mut() {
                e.caret = caret.min(n);
                if !extend {
                    e.anchor = e.caret;
                }
            }
        };
        handled[k] = true;
        match ev {
            egui::Event::Text(s) | egui::Event::Paste(s) => insert(app, s),
            // A bare line break from the IME is the Enter key, which the key arm handles.
            egui::Event::Ime(egui::ImeEvent::Preedit { text: s, .. } | egui::ImeEvent::Commit(s)) if s == "\n" || s == "\r" => handled[k] = false,
            egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => ime_update(app, text, false),
            egui::Event::Ime(egui::ImeEvent::Commit(s)) => ime_update(app, s, true),
            egui::Event::Ime(_) => {}
            egui::Event::Copy | egui::Event::Cut => {
                if a < b {
                    ctx.copy_text(text.chars().skip(a).take(b - a).collect());
                    if matches!(ev, egui::Event::Cut) {
                        insert(app, "");
                    }
                }
            }
            egui::Event::Key { key, pressed: true, modifiers: m, .. } => {
                use egui::Key;
                let key = &flow_key(*key, vertical_flow);
                match key {
                    Key::Backspace | Key::Delete => {
                        if a == b {
                            let fwd = *key == Key::Delete;
                            let to = match (fwd, m.alt, m.command) {
                                (false, _, true) => line_edge(app, id, a, false),
                                (false, true, _) => word_boundary(&text, a, false),
                                (false, _, _) => a.saturating_sub(1),
                                (true, true, _) => word_boundary(&text, a, true),
                                (true, _, _) => (a + 1).min(n),
                            };
                            if to != a {
                                if let Some(e) = app.ui.text_edit.as_mut() {
                                    e.anchor = to;
                                }
                                insert(app, "");
                            }
                        } else {
                            insert(app, "");
                        }
                    }
                    // Photoshop: Alt/Option+←/→ at a collapsed caret kerns the pair before it by
                    // 20/1000 em (100 with ⌘/Ctrl), one history step per press.
                    Key::ArrowLeft | Key::ArrowRight if m.alt && !m.shift && a == b => {
                        let step = if m.command { 100.0 } else { 20.0 };
                        kern_pair(app, id, ed.caret, if *key == Key::ArrowRight { step } else { -step });
                    }
                    Key::ArrowLeft | Key::ArrowRight => {
                        let fwd = *key == Key::ArrowRight;
                        // Word movement: ⌘/Ctrl (Photoshop's; Alt is kerning), and Alt+Shift
                        // extends the selection by words; Home/End go to the line edges.
                        let to = if m.command || m.alt {
                            word_boundary(&text, ed.caret, fwd)
                        } else if a != b && !m.shift {
                            if fwd { b } else { a }
                        } else if fwd {
                            ed.caret + 1
                        } else {
                            ed.caret.saturating_sub(1)
                        };
                        set(app, to, m.shift);
                    }
                    Key::ArrowUp | Key::ArrowDown => {
                        let to = if m.command {
                            if *key == Key::ArrowUp { 0 } else { n }
                        } else {
                            line_step(app, id, ed.caret, if *key == Key::ArrowUp { -1 } else { 1 })
                        };
                        set(app, to, m.shift);
                    }
                    Key::Home | Key::End => {
                        let to = line_edge(app, id, ed.caret, *key == Key::End);
                        set(app, to, m.shift);
                    }
                    Key::Enter if m.command => commit(app),
                    Key::Enter => insert(app, "\n"),
                    Key::Escape => commit(app),
                    Key::A if m.command => {
                        if let Some(e) = app.ui.text_edit.as_mut() {
                            e.anchor = 0;
                            e.caret = n;
                        }
                    }
                    // Other command shortcuts (⌘Z, ⌘S, …) pass through to the menus.
                    _ if m.command => handled[k] = false,
                    _ => {}
                }
            }
            egui::Event::Key { pressed: false, modifiers: m, .. } => handled[k] = !m.command,
            _ => handled[k] = false,
        }
    }
    if handled.iter().any(|h| *h) {
        let mut k = 0;
        ctx.input_mut(|i| {
            i.events.retain(|_| {
                let keep = !handled.get(k).copied().unwrap_or(false);
                k += 1;
                keep
            })
        });
    }
    app.ui.text_edit.is_some()
}

/// End the editing session. A new layer left empty is deleted; a new layer is named after its text.
pub fn commit(app: &mut PhotocraftApp) {
    let Some(ed) = app.ui.text_edit.take() else { return };
    let Some(text) = current_text(app, LayerId(ed.layer)) else { return };
    if text.trim().is_empty() && ed.created {
        let _ = app.run("layer.delete", json!({"layer": ed.layer}));
    } else if ed.created {
        let name: String = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(40).collect();
        let _ = app.run("type.edit", json!({"layer": ed.layer, "name": name, "coalesce": ed.session}));
    }
}

/// Selection highlight, caret and text frame over the canvas.
pub fn draw_overlay(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    let Some(ed) = app.ui.text_edit.clone() else { return };
    let id = LayerId(ed.layer);
    let Some((l, aff, text)) = layout(app, id) else { return };
    let t = crate::theme::Tokens::get(painter.ctx());
    // Text space → screen.
    let scr_t = |x: f32, y: f32| -> Pos2 {
        let p = aff.apply(Point::new(x as f64, y as f64));
        xf.to_screen(p.x as f32, p.y as f32)
    };
    // Line space (lines, clusters, carets) → screen: vertical type turns it 90° clockwise.
    let scr = |x: f32, y: f32| -> Pos2 {
        let (x, y) = l.to_text(x, y);
        scr_t(x, y)
    };
    // Tell the OS where the caret is: this is what enables the IME and places its candidate window.
    {
        let (x, top, bot) = l.caret(byte_of(&text, ed.caret));
        let (x, top, bot) = if l.lines.is_empty() { (0.0, -(12.0 * l.px_per_pt.max(1.0)), 3.0) } else { (x, top, bot) };
        let r = egui::Rect::from_two_pos(scr(x, top), scr(x, bot)).expand(1.0);
        painter.ctx().output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect: r, cursor_rect: r, should_interrupt_composition: false });
        });
    }
    // Frame: paragraph text shows its box with handles; point text an underline per line.
    let shape = app.session.active().and_then(|s| text_layer(&s.doc, id).map(|t| t.shape));
    let frame = Stroke::new(1.0, t.accent);
    match shape {
        Some(photocraft_doc::text::TextShape::Box { x, y, width, height }) => {
            let c = [scr_t(x, y), scr_t(x + width, y), scr_t(x + width, y + height), scr_t(x, y + height)];
            painter.add(egui::Shape::closed_line(c.to_vec(), frame));
            let mids = [c[0].lerp(c[1], 0.5), c[1].lerp(c[2], 0.5), c[2].lerp(c[3], 0.5), c[3].lerp(c[0], 0.5)];
            for p in c.iter().chain(mids.iter()) {
                let r = egui::Rect::from_center_size(*p, egui::vec2(7.0, 7.0));
                painter.rect_filled(r, 0.0, Color32::WHITE);
                painter.rect_stroke(r, 0.0, frame, egui::StrokeKind::Inside);
            }
        }
        _ => {
            for ln in &l.lines {
                // Horizontal: just under the baseline; vertical: the column's centre line.
                let y = if l.vertical { ln.baseline } else { ln.baseline + ln.descent * 0.25 };
                painter.line_segment([scr(ln.x0.min(0.0), y), scr(ln.x1.max(ln.x0 + 1.0), y)], Stroke::new(1.0, t.accent.gamma_multiply(0.8)));
            }
        }
    }
    // Uncommitted IME text is underlined.
    if let Some((ps, pl)) = ed.preedit {
        let (a, b) = (byte_of(&text, ps), byte_of(&text, ps + pl));
        for (li, ln) in l.lines.iter().enumerate() {
            let xs: Vec<(f32, f32)> =
                l.clusters.iter().filter(|c| c.line == li && c.range.start >= a && c.range.end <= b).map(|c| (c.x, c.x + c.advance)).collect();
            let (x0, x1) = xs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), (p, q)| (lo.min(*p), hi.max(*q)));
            if x0 < x1 {
                let y = ln.baseline + ln.descent * 0.5;
                painter.line_segment([scr(x0, y), scr(x1, y)], Stroke::new(1.5, t.accent));
            }
        }
    }
    // Selection: per line, the clusters inside [a, b).
    let (a, b) = (byte_of(&text, ed.caret.min(ed.anchor)), byte_of(&text, ed.caret.max(ed.anchor)));
    if a < b {
        let fill = Color32::from_rgba_unmultiplied(t.accent.r(), t.accent.g(), t.accent.b(), 110);
        for (li, ln) in l.lines.iter().enumerate() {
            let xs: Vec<(f32, f32)> =
                l.clusters.iter().filter(|c| c.line == li && c.range.start >= a && c.range.end <= b).map(|c| (c.x, c.x + c.advance)).collect();
            let (mut x0, mut x1) = xs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), (p, q)| (lo.min(*p), hi.max(*q)));
            // A selected line break shows as a small sliver past the line end.
            if b > ln.range.end && a <= ln.range.end {
                x1 = x1.max(ln.x1 + (ln.ascent + ln.descent) * 0.25);
                x0 = x0.min(ln.x1);
            }
            if x0 < x1 {
                let (top, bot) = (ln.baseline - ln.ascent, ln.baseline + ln.descent);
                painter.add(egui::Shape::convex_polygon(vec![scr(x0, top), scr(x1, top), scr(x1, bot), scr(x0, bot)], fill, Stroke::NONE));
            }
        }
    } else {
        // Blinking caret (Photoshop's ~0.53 s rhythm), solid while dragging.
        let time = painter.ctx().input(|i| i.time);
        if ed.dragging || (time * 1000.0 / 530.0) as i64 % 2 == 0 {
            let (x, top, bot) = l.caret(a);
            let (x, top, bot) = if l.lines.is_empty() { (0.0, -(12.0 * l.px_per_pt.max(1.0)), 3.0) } else { (x, top, bot) };
            let c = if crate::theme::Tokens::get(painter.ctx()).pro { Color32::WHITE } else { Color32::BLACK };
            painter.line_segment([scr(x, top), scr(x, bot)], Stroke::new(1.5, c));
            painter.line_segment([scr(x, top), scr(x, bot)], Stroke::new(0.75, Color32::BLACK));
        }
    }
}

/// Font families (bundled + system), cached for the process.
pub fn families() -> &'static [String] {
    static FAMILIES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FAMILIES.get_or_init(|| photocraft_text::shared().lock().map(|mut e| e.fonts.families()).unwrap_or_default())
}

fn weight_name(w: f32) -> &'static str {
    match w.round() as i32 {
        ..=150 => "Thin",
        151..=250 => "ExtraLight",
        251..=350 => "Light",
        351..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "SemiBold",
        651..=750 => "Bold",
        751..=850 => "ExtraBold",
        _ => "Black",
    }
}

/// Style names ("Regular", "Bold Italic", …) available for a family.
pub fn styles(family: &str) -> Vec<String> {
    let faces = photocraft_text::shared().lock().map(|mut e| e.fonts.faces(family)).unwrap_or_default();
    let mut v: Vec<(i32, bool, String)> = faces
        .iter()
        .map(|f| {
            let w = weight_name(f.weight);
            let name = match (w, f.italic) {
                ("Regular", true) => "Italic".to_string(),
                (w, true) => format!("{w} Italic"),
                (w, false) => w.to_string(),
            };
            (f.weight.round() as i32, f.italic, name)
        })
        .collect();
    v.sort();
    v.dedup_by(|a, b| a.2 == b.2);
    let v: Vec<String> = v.into_iter().map(|x| x.2).collect();
    if v.is_empty() { vec![tl!("Regular").into()] } else { v }
}

/// Searchable font-family combo box.
fn font_picker(ui: &mut egui::Ui, current: &mut String, width: f32) -> bool {
    let mut changed = false;
    let search_id = ui.id().with("font-search");
    egui::ComboBox::from_id_salt("type-font").selected_text(current.as_str()).width(width).height(460.0).icon(crate::widgets::chevron_icon).show_ui(ui, |ui| {
        let mut q: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
        let r = ui.add(egui::TextEdit::singleline(&mut q).hint_text(tl!("Search fonts")).desired_width(200.0));
        if !r.has_focus() && q.is_empty() {
            r.request_focus();
        }
        ui.data_mut(|d| d.insert_temp(search_id, q.clone()));
        let ql = q.to_lowercase();
        for f in families().iter().filter(|f| ql.is_empty() || f.to_lowercase().contains(&ql)) {
            if ui.selectable_label(f == current, f).clicked() {
                *current = f.clone();
                changed = true;
                ui.data_mut(|d| d.remove::<String>(search_id));
            }
        }
    });
    changed
}

/// The type layer the options bar edits: the one being edited, else the active layer if it is type.
fn target(app: &PhotocraftApp) -> Option<(u64, Option<[usize; 2]>)> {
    if let Some(ed) = &app.ui.text_edit {
        let (a, b) = (ed.caret.min(ed.anchor), ed.caret.max(ed.anchor));
        return Some((ed.layer, (a < b).then_some([a, b])));
    }
    let st = app.session.active()?;
    let id = st.active_layer?;
    text_layer(&st.doc, id).map(|_| (id.0, None))
}

/// Scale of the target layer's transform. Like Photoshop, sizes show and edit as the layer
/// appears: a 12 pt layer scaled 200% (common in PSDs) reads 24 pt.
fn shown_scale(app: &PhotocraftApp) -> f32 {
    let Some((id, _)) = target(app) else { return 1.0 };
    let Some(t) = app.session.active().and_then(|st| text_layer(&st.doc, LayerId(id))) else { return 1.0 };
    let m = t.transform.m;
    let k = (m[0] * m[3] - m[1] * m[2]).abs().sqrt() as f32;
    if k.is_finite() && k > 1e-3 { k } else { 1.0 }
}

/// Coalesce key while a field is being scrubbed: every step of one drag shares one history step
/// (Photoshop's single step per scrub, #124). Unique per press, so two drags are two steps.
fn drag_key(ctx: &egui::Context) -> Option<String> {
    let id = ctx.dragged_id()?;
    let t = ctx.input(|i| i.pointer.press_start_time())?;
    Some(format!("type-drag-{}-{}", id.value(), t.to_bits()))
}

/// Apply character/paragraph properties to the target (selection, else whole layer) and remember
/// them as tool defaults.
fn apply(app: &mut PhotocraftApp, ctx: &egui::Context, props: serde_json::Value) {
    let Some((layer, range)) = target(app) else { return };
    let mut p = props;
    p["layer"] = json!(layer);
    if let Some(r) = range {
        p["range"] = json!(r);
    }
    if let Some(key) = app.ui.text_edit.as_ref().map(|ed| ed.session.clone()).or_else(|| drag_key(ctx)) {
        p["coalesce"] = json!(key);
    }
    let _ = app.run("type.setStyle", p);
}

/// Photoshop's Type options bar.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    // Show the target layer's (first-run) style, else the tool defaults.
    let shown = target(app).and_then(|(id, _)| {
        let st = app.session.active()?;
        let tl = text_layer(&st.doc, LayerId(id))?;
        let run = tl.char_runs().into_iter().next().map(|r| r.style);
        // The size at the selection (else the first run), as the layer shows it (#124).
        let size = styles_at(app).map_or(tl.size_pt, |(c, _)| c.size_pt) * shown_scale(app);
        Some((tl.font_family.clone(), run.as_ref().map(|s| s.font_style.clone()).unwrap_or_default(), size, tl.color))
    });
    let o = app.ui.tool_options.clone();
    let (mut fam, mut style, mut size) = match &shown {
        Some((f, s, z, _)) => (f.clone(), if s.is_empty() { "Regular".into() } else { s.clone() }, *z),
        None => (o.type_font.clone(), o.type_style.clone(), o.type_size),
    };
    if crate::icons::button(ui, "text-cursor", 24.0, false, tl!("Toggle text orientation")).clicked()
        && let Some((layer, _)) = target(app)
    {
        let to = if is_vertical(app, LayerId(layer)) { "horizontal" } else { "vertical" };
        if let Err(e) = app.run(&format!("type.orientation.{to}"), json!({"layer": layer})) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
    if font_picker(ui, &mut fam, 170.0) {
        app.ui.tool_options.type_font = fam.clone();
        let st = styles(&fam);
        style = if st.contains(&style) { style } else { st.first().cloned().unwrap_or_else(|| "Regular".into()) };
        app.ui.tool_options.type_style = style.clone();
        apply(app, ui.ctx(), json!({"font": fam, "fontStyle": style}));
    }
    let opts: Vec<(String, String)> = styles(&fam).into_iter().map(|s| (s.clone(), s)).collect();
    let opts_ref: Vec<(String, &str)> = opts.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
    if crate::widgets::dropdown(ui, "type-style", &mut style, &opts_ref, 110.0) {
        app.ui.tool_options.type_style = style.clone();
        apply(app, ui.ctx(), json!({"fontStyle": style}));
    }
    let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 22.0), egui::Sense::hover());
    crate::icons::paint(ui, r, "type", 13.0, t.icon);
    if crate::widgets::value_field(ui, &mut size, 1.0..=1296.0, "pt", 66.0).changed() {
        app.ui.tool_options.type_size = size;
        let k = shown_scale(app);
        apply(app, ui.ctx(), json!({"size": size / k}));
    }
    let mut aa = o.type_aa.clone();
    let aa_opts = [
        ("none".to_string(), tl!("None")),
        ("sharp".to_string(), tl!("Sharp")),
        ("crisp".to_string(), tl!("Crisp")),
        ("strong".to_string(), tl!("Strong")),
        ("smooth".to_string(), tl!("Smooth")),
    ];
    if crate::widgets::dropdown(ui, "type-aa", &mut aa, &aa_opts, 80.0) {
        app.ui.tool_options.type_aa = aa.clone();
        if let Some((layer, _)) = target(app) {
            let mut p = json!({"layer": layer, "antialias": aa});
            if let Some(ed) = &app.ui.text_edit {
                p["coalesce"] = json!(ed.session);
            }
            let _ = app.run("type.edit", p);
        }
    }
    crate::widgets::vline(ui, 22.0);
    ui.spacing_mut().item_spacing.x = 2.0;
    for (align, icon, tip) in
        [("left", "align-left", tl!("Left align text")), ("center", "align-center", tl!("Center text")), ("right", "align-right", tl!("Right align text"))]
    {
        if crate::icons::button(ui, icon, 24.0, o.type_align == align, tip).clicked() {
            app.ui.tool_options.type_align = align.into();
            apply(app, ui.ctx(), json!({"align": align}));
        }
    }
    ui.spacing_mut().item_spacing.x = 8.0;
    crate::widgets::vline(ui, 22.0);
    // Text colour swatch with a picker popup.
    let c = shown
        .as_ref()
        .map(|s| s.3)
        .map(|c| {
            let v = c.to_rgb();
            [v[0], v[1], v[2], 1.0]
        })
        .unwrap_or(app.session.tools.foreground);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 18.0), egui::Sense::click());
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(q(c[0]), q(c[1]), q(c[2])));
    ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
    let resp = resp.on_hover_text(tl!("Set the text color"));
    egui::Popup::from_toggle_button_response(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        let mut col = Color32::from_rgb(q(c[0]), q(c[1]), q(c[2]));
        if egui::color_picker::color_picker_color32(ui, &mut col, egui::color_picker::Alpha::Opaque) {
            apply(app, ui.ctx(), json!({"color": format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b())}));
        }
    });
    if app.ui.text_edit.is_some() {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if crate::icons::button(
                ui,
                "check",
                24.0,
                false,
                &crate::i18n::fmt(tl!("Commit any current edits ({key})"), &[("key", &crate::shortcuts::pretty("Cmd+Enter"))]),
            )
            .clicked()
            {
                commit(app);
            }
            if crate::icons::button(ui, "ban", 24.0, false, tl!("Cancel any current edits (Esc)")).clicked() {
                cancel(app);
            }
        });
    }
}

/// Character and paragraph style at the target (selection start, else the layer's first run).
fn styles_at(app: &PhotocraftApp) -> Option<(photocraft_doc::text::CharStyle, photocraft_doc::text::ParagraphStyle)> {
    let (layer, range) = target(app)?;
    let st = app.session.active()?;
    let t = text_layer(&st.doc, LayerId(layer))?;
    let at = range.map_or(0, |r| byte_of(&t.text, r[0]));
    let pick = |lens: Vec<usize>| -> usize {
        let mut acc = 0;
        for (i, len) in lens.iter().enumerate() {
            acc += len;
            if at < acc {
                return i;
            }
        }
        lens.len().saturating_sub(1)
    };
    let runs = t.char_runs();
    let paras = t.paragraph_runs();
    let c = runs.get(pick(runs.iter().map(|r| r.len).collect())).map(|r| r.style.clone())?;
    let p = paras.get(pick(paras.iter().map(|r| r.len).collect())).map(|r| r.style.clone()).unwrap_or_default();
    Some((c, p))
}

/// Kerning shown for the target: at a collapsed caret, the pair before it (Photoshop's
/// Character panel); else the style at the selection.
fn kerning_at(app: &PhotocraftApp) -> Option<(photocraft_doc::text::Kerning, f32)> {
    let ed = app.ui.text_edit.as_ref().filter(|e| e.caret == e.anchor && e.caret > 0)?;
    let st = app.session.active()?;
    let t = text_layer(&st.doc, LayerId(ed.layer))?;
    let at = byte_of(&t.text, ed.caret - 1);
    let mut end = 0;
    t.char_runs()
        .into_iter()
        .find(|r| {
            end += r.len;
            at < end
        })
        .map(|r| (r.style.kerning, r.style.kern))
}

/// The Character panel's kerning text: "Metrics", "Optical" or the manual value.
pub(crate) fn kerning_label((mode, kern): (photocraft_doc::text::Kerning, f32)) -> String {
    use photocraft_doc::text::Kerning;
    match mode {
        _ if kern != 0.0 && kern.is_finite() => format!("{}", kern.round() as i64),
        Kerning::Metrics => "Metrics".into(),
        Kerning::Optical => "Optical".into(),
        Kerning::Off => "0".into(),
    }
}

/// Parses a typed kerning value: Metrics / Optical (any case) or a number in 1/1000 em
/// (Photoshop's range, -1000..10000). `None` for anything else.
pub(crate) fn parse_kerning(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "metrics" => Some(json!("metrics")),
        "optical" => Some(json!("optical")),
        _ => s.parse::<f64>().ok().filter(|v| v.is_finite() && (-1000.0..=10000.0).contains(v)).map(|v| json!(v.round())),
    }
}

/// Photoshop's editable kerning combo: type a value or Metrics/Optical, or pick a preset.
fn kerning_field(ui: &mut egui::Ui, shown: &str, width: f32) -> Option<serde_json::Value> {
    let id = ui.make_persistent_id("props-kern-text");
    let mut buf: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| shown.to_string());
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width((width - 40.0).max(24.0)).id(id.with("edit")));
        if resp.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, buf.clone()));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
        if resp.lost_focus() && buf.trim() != shown {
            out = parse_kerning(&buf);
        }
        let presets = ["Metrics", "Optical", "0", "-100", "-75", "-50", "-25", "-10", "-5", "5", "10", "25", "50", "75", "100", "200"];
        egui::ComboBox::from_id_salt("props-kern-presets").selected_text("").width(16.0).height(420.0).icon(crate::widgets::chevron_icon).show_ui(ui, |ui| {
            for p in presets {
                if ui.selectable_label(p == shown, p).clicked() {
                    out = parse_kerning(p);
                }
            }
        });
    });
    out
}

/// Applies a kerning value: at a collapsed caret to the pair before it, else to the selection
/// (or the whole layer).
fn apply_kerning(app: &mut PhotocraftApp, ctx: &egui::Context, v: serde_json::Value) {
    match app.ui.text_edit.clone().filter(|e| e.caret == e.anchor) {
        Some(ed) if ed.caret > 0 => {
            let _ = app.run("type.edit", json!({"layer": ed.layer, "range": [ed.caret - 1, ed.caret], "kerning": v}));
        }
        Some(_) => {}
        None => apply(app, ctx, json!({ "kerning": v })),
    }
}

fn icon_label(ui: &mut egui::Ui, icon: &str, tip: &str) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(egui::vec2(crate::props_layout::LABEL_W, 22.0), egui::Sense::hover());
    crate::icons::paint(ui, r, icon, 12.0, t.text_dim);
    resp.on_hover_text(tip);
}

/// Labelled numeric field (Photoshop's icon + value pairs). Returns the new value when edited.
fn num_field(ui: &mut egui::Ui, label: &str, tip: &str, v: f32, range: std::ops::RangeInclusive<f32>, unit: &str, width: f32) -> Option<f32> {
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut v = v;
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (r, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, crate::theme::semibold(10.0), t.text_dim);
        resp.on_hover_text(tip);
        if crate::widgets::value_field(ui, &mut v, range, unit, width).changed() {
            out = Some(v);
        }
    });
    out
}

/// Photoshop's paragraph alignment glyphs: four text lines whose widths/offsets show the mode
/// (justify variants run full width, with the last line placed left/centre/right).
fn align_glyph(p: &egui::Painter, r: egui::Rect, a: photocraft_doc::text::TextAlign, c: Color32) {
    use photocraft_doc::text::TextAlign as A;
    let widths = [1.0, 0.62, 1.0, 0.62];
    for (i, w) in widths.iter().enumerate() {
        let y = r.top() + i as f32 * r.height() / 3.0;
        let full = r.width();
        let (len, x0) = match a {
            A::Left => (full * w, r.left()),
            A::Center => (full * w, r.center().x - full * w / 2.0),
            A::Right => (full * w, r.right() - full * w),
            A::JustifyAll => (full, r.left()),
            _ if i < 3 => (full, r.left()),
            A::JustifyLeft => (full * 0.55, r.left()),
            A::JustifyCenter => (full * 0.55, r.center().x - full * 0.275),
            _ => (full * 0.55, r.right() - full * 0.55),
        };
        p.line_segment([egui::pos2(x0, y), egui::pos2(x0 + len, y)], Stroke::new(1.5, c));
    }
}

/// A toggle button (faux styles, paragraph alignment) of width `w`, painted by `paint`.
fn toggle_cell(ui: &mut egui::Ui, w: f32, on: bool, tip: &str, paint: impl FnOnce(&egui::Ui, egui::Rect, Color32)) -> bool {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    let bg = if on {
        t.accent_soft
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, t.radius_sm, bg);
    if on {
        ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.accent_border), egui::StrokeKind::Inside);
    }
    paint(ui, r, if on { t.text } else { t.text_dim });
    let tip = tip.to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, &tip));
    resp.on_hover_text(tl!(&tip)).clicked()
}

/// Width of each of `n` equal cells filling `avail` with 2 pt gaps.
fn cell_width(avail: f32, n: usize) -> f32 {
    let n = n.max(1) as f32;
    ((avail - (n - 1.0) * 2.0) / n).floor().max(20.0)
}

/// Properties panel sections for a type layer: Character, Paragraph and Type Options (#155).
pub fn type_properties(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    type_sections(app, ui, true, true);
    type_options(app, ui);
}

/// Window › Character / Paragraph (#150): the same controls as Properties, in their own dock
/// group. Without a type layer (or a type edit) there is nothing to show.
pub fn character_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui, paragraph: bool) {
    if styles_at(app).is_none() {
        let t = crate::theme::Tokens::get(ui.ctx());
        ui.add_space(8.0);
        ui.label(egui::RichText::new(tl!("Select a type layer to edit its character and paragraph settings.")).color(t.text_dim));
        return;
    }
    type_sections(app, ui, !paragraph, paragraph);
}

/// The Character and/or Paragraph sections, with the shared collapsible headers and fields that
/// fill the panel width (#155).
fn type_sections(app: &mut PhotocraftApp, ui: &mut egui::Ui, character: bool, paragraph: bool) {
    use crate::props_layout::{COL_GAP, LABEL_GAP, LABEL_W, field_width, section};
    use crate::theme::ROW_GAP;
    let Some((c, para)) = styles_at(app) else { return };
    let t = crate::theme::Tokens::get(ui.ctx());
    // Row helper: `ui.horizontal` with the column gap, then the shared row gap.
    let row = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = COL_GAP;
            add(ui)
        });
        ui.add_space(ROW_GAP);
    };
    if character && section(ui, "character", tl!("Character")) {
        let full = ui.available_width();
        let w = field_width(full, 2, LABEL_W);
        let mut fam = c.font_family.clone();
        row(ui, &mut |ui| {
            if font_picker(ui, &mut fam, full) {
                app.ui.tool_options.type_font = fam.clone();
                let st = styles(&fam);
                let style = if st.contains(&c.font_style) { c.font_style.clone() } else { st.first().cloned().unwrap_or_else(|| tl!("Regular").into()) };
                apply(app, ui.ctx(), json!({"font": fam, "fontStyle": style}));
            }
        });
        row(ui, &mut |ui| {
            let mut style = if c.font_style.is_empty() { "Regular".to_string() } else { c.font_style.clone() };
            let opts: Vec<(String, String)> = styles(&fam).into_iter().map(|s| (s.clone(), s)).collect();
            let opts_ref: Vec<(String, &str)> = opts.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
            if crate::widgets::dropdown(ui, "props-type-style", &mut style, &opts_ref, full) {
                apply(app, ui.ctx(), json!({"fontStyle": style}));
            }
        });
        row(ui, &mut |ui| {
            let k = shown_scale(app);
            if let Some(v) = num_field(ui, "tT", "Font size", c.size_pt * k, 0.1..=1296.0, "pt", w) {
                app.ui.tool_options.type_size = v;
                apply(app, ui.ctx(), json!({"size": v / k}));
            }
            let lead = c.leading_pt.unwrap_or(c.size_pt * para.auto_leading.max(0.01)) * k;
            if let Some(v) = num_field(ui, "A↕", "Leading (set to the font size × auto-leading when Auto)", lead, 0.1..=5000.0, "pt", w) {
                apply(app, ui.ctx(), json!({"leading": v / k}));
            }
        });
        row(ui, &mut |ui| {
            let shown = kerning_label(kerning_at(app).unwrap_or((c.kerning, c.kern)));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = LABEL_GAP;
                icon_label(ui, "text-cursor", "Kerning: Metrics, Optical or a value in 1/1000 em (Alt+←/→ at the caret)");
                if let Some(v) = kerning_field(ui, &shown, w) {
                    apply_kerning(app, ui.ctx(), v);
                }
            });
            if let Some(v) = num_field(ui, "VA", "Tracking (1/1000 em)", c.tracking, -1000.0..=10000.0, "", w) {
                apply(app, ui.ctx(), json!({"tracking": v}));
            }
        });
        row(ui, &mut |ui| {
            if let Some(v) = num_field(ui, "↕T", "Vertical scale", c.vertical_scale * 100.0, 0.0..=1000.0, "%", w) {
                apply(app, ui.ctx(), json!({"verticalScale": v}));
            }
            if let Some(v) = num_field(ui, "↔T", "Horizontal scale", c.horizontal_scale * 100.0, 0.0..=1000.0, "%", w) {
                apply(app, ui.ctx(), json!({"horizontalScale": v}));
            }
        });
        row(ui, &mut |ui| {
            if let Some(v) = num_field(ui, "Aª", "Baseline shift", c.baseline_shift_pt, -1296.0..=1296.0, "pt", w) {
                apply(app, ui.ctx(), json!({"baselineShift": v}));
            }
            // Colour: label + a swatch filling the rest of the second column.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = LABEL_GAP;
                let g = ui.painter().layout_no_wrap(tl!("Color").into(), egui::FontId::proportional(12.0), t.text_dim);
                let lw = g.size().x.max(LABEL_W);
                let (lr, _) = ui.allocate_exact_size(egui::vec2(lw, 22.0), egui::Sense::hover());
                ui.painter().galley(egui::pos2(lr.left(), lr.center().y - g.size().y / 2.0), g, t.text_dim);
                let rgb = c.color.to_rgb();
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                let sw = (LABEL_W + LABEL_GAP + w - lw - LABEL_GAP).max(24.0);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(sw, 22.0), egui::Sense::click());
                ui.painter().rect_filled(rect, t.radius_sm, Color32::from_rgb(q(rgb[0]), q(rgb[1]), q(rgb[2])));
                ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
                let resp = resp.on_hover_text(tl!("Text color"));
                egui::Popup::from_toggle_button_response(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    let mut col = Color32::from_rgb(q(rgb[0]), q(rgb[1]), q(rgb[2]));
                    if egui::color_picker::color_picker_color32(ui, &mut col, egui::color_picker::Alpha::Opaque) {
                        apply(app, ui.ctx(), json!({"color": format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b())}));
                    }
                });
            });
        });
        // Faux styles: T (bold)  T (italic)  TT  Tᴛ  T̲  T̶, sharing the row evenly.
        let caps = c.caps;
        let toggles: [(&str, &str, bool, serde_json::Value); 6] = [
            ("T", "Faux Bold", c.faux_bold, json!({"fauxBold": !c.faux_bold})),
            ("T", "Faux Italic", c.faux_italic, json!({"fauxItalic": !c.faux_italic})),
            (
                "TT",
                tl!("All Caps"),
                caps == photocraft_doc::text::Caps::AllCaps,
                json!({"caps": if caps == photocraft_doc::text::Caps::AllCaps { "normal" } else { "allCaps" }}),
            ),
            (
                "Tᴛ",
                tl!("Small Caps"),
                caps == photocraft_doc::text::Caps::SmallCaps,
                json!({"caps": if caps == photocraft_doc::text::Caps::SmallCaps { "normal" } else { "smallCaps" }}),
            ),
            ("T", "Underline", c.underline, json!({"underline": !c.underline})),
            ("T", "Strikethrough", c.strikethrough, json!({"strikethrough": !c.strikethrough})),
        ];
        let cw = cell_width(full, toggles.len());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (i, (glyph, tip, on, props)) in toggles.into_iter().enumerate() {
                let clicked = toggle_cell(ui, cw, on, tip, |ui, r, col| {
                    let font = if i == 0 { crate::theme::semibold(13.0) } else { egui::FontId::proportional(13.0) };
                    let g = ui.painter().layout_no_wrap(glyph.to_string(), font, col);
                    let pos = r.center() - g.size() / 2.0;
                    let gr = egui::Rect::from_min_size(pos, g.size());
                    if i == 1 {
                        // Faux italic: a slanted T drawn as strokes (no italic face is bundled).
                        let (top, bot, cx) = (gr.top() + 3.0, gr.bottom() - 3.0, gr.center().x);
                        let slant = (bot - top) * 0.25;
                        ui.painter().line_segment([egui::pos2(cx - 4.0 + slant / 2.0, top), egui::pos2(cx + 4.0 + slant / 2.0, top)], Stroke::new(1.3, col));
                        ui.painter().line_segment([egui::pos2(cx + slant / 2.0, top), egui::pos2(cx - slant / 2.0, bot)], Stroke::new(1.3, col));
                    } else {
                        ui.painter().galley(pos, g, col);
                    }
                    if i == 4 {
                        ui.painter().line_segment([egui::pos2(gr.left(), gr.bottom() - 2.0), egui::pos2(gr.right(), gr.bottom() - 2.0)], Stroke::new(1.0, col));
                    }
                    if i == 5 {
                        ui.painter()
                            .line_segment([egui::pos2(gr.left() - 1.0, gr.center().y), egui::pos2(gr.right() + 1.0, gr.center().y)], Stroke::new(1.0, col));
                    }
                });
                if clicked {
                    apply(app, ui.ctx(), props);
                }
            }
        });
        ui.add_space(ROW_GAP);
    }
    if paragraph && section(ui, "paragraph", tl!("Paragraph")) {
        use photocraft_doc::text::TextAlign as A;
        let full = ui.available_width();
        let w = field_width(full, 2, LABEL_W);
        let items = [
            ("left", tl!("Left align text"), A::Left),
            ("center", tl!("Center text"), A::Center),
            ("right", tl!("Right align text"), A::Right),
            ("justifyLeft", tl!("Justify last left"), A::JustifyLeft),
            ("justifyCenter", tl!("Justify last centered"), A::JustifyCenter),
            ("justifyRight", tl!("Justify last right"), A::JustifyRight),
            ("justifyAll", tl!("Justify all"), A::JustifyAll),
        ];
        let cw = cell_width(full, items.len());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (key, tip, a) in items {
                let clicked = toggle_cell(ui, cw, para.align == a, tip, |ui, r, col| {
                    // The glyph keeps its size; the cell grows around it.
                    let g = egui::Rect::from_center_size(r.center(), egui::vec2(12.0, 10.0));
                    align_glyph(ui.painter(), g, a, if para.align == a { col } else { t.icon });
                });
                if clicked {
                    apply(app, ui.ctx(), json!({"align": key}));
                }
            }
        });
        ui.add_space(ROW_GAP);
        row(ui, &mut |ui| {
            if let Some(v) = num_field(ui, "→|", "Indent left margin", para.start_indent_pt, -1296.0..=1296.0, "pt", w) {
                apply(app, ui.ctx(), json!({"startIndent": v}));
            }
            if let Some(v) = num_field(ui, "|←", "Indent right margin", para.end_indent_pt, -1296.0..=1296.0, "pt", w) {
                apply(app, ui.ctx(), json!({"endIndent": v}));
            }
        });
        row(ui, &mut |ui| {
            if let Some(v) = num_field(ui, "¶→", "Indent first line", para.first_line_indent_pt, -1296.0..=1296.0, "pt", w) {
                apply(app, ui.ctx(), json!({"firstLineIndent": v}));
            }
        });
        row(ui, &mut |ui| {
            if let Some(v) = num_field(ui, "↑¶", "Add space before paragraph", para.space_before_pt, 0.0..=1296.0, "pt", w) {
                apply(app, ui.ctx(), json!({"spaceBefore": v}));
            }
            if let Some(v) = num_field(ui, "¶↓", "Add space after paragraph", para.space_after_pt, 0.0..=1296.0, "pt", w) {
                apply(app, ui.ctx(), json!({"spaceAfter": v}));
            }
        });
        let mut hy = para.hyphenate;
        if crate::widgets::checkbox(ui, &mut hy, "Hyphenate").changed() {
            apply(app, ui.ctx(), json!({"hyphenate": hy}));
        }
        ui.add_space(ROW_GAP);
    }
}

/// Type Options: anti-aliasing and orientation of the whole layer (the Type menu's commands).
fn type_options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    use crate::props_layout::{COL_GAP, LABEL_GAP};
    let Some((layer, _)) = target(app) else { return };
    let Some((aa, vertical)) = app
        .session
        .active()
        .and_then(|st| text_layer(&st.doc, LayerId(layer)))
        .map(|t| (photocraft_engine::type_extra_cmds::aa_name(t.antialias).to_string(), t.orientation == photocraft_doc::text::Orientation::Vertical))
    else {
        return;
    };
    if !crate::props_layout::section(ui, "typeOptions", "Type Options") {
        return;
    }
    let full = ui.available_width();
    let mut run: Option<String> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = COL_GAP;
        // Anti-aliasing dropdown (aₐ glyph label), then the two orientation toggles.
        let orient_w = 2.0 * 28.0 + 2.0;
        let dd_w = (full - crate::props_layout::LABEL_W - LABEL_GAP - COL_GAP - orient_w).max(60.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = LABEL_GAP;
            let t = crate::theme::Tokens::get(ui.ctx());
            let (r, resp) = ui.allocate_exact_size(egui::vec2(crate::props_layout::LABEL_W, 22.0), egui::Sense::hover());
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "aa", crate::theme::semibold(11.0), t.text_dim);
            resp.on_hover_text(tl!("Anti-aliasing method"));
            let mut cur = aa.clone();
            let opts: Vec<(String, &str)> = ["none", "sharp", "crisp", "strong", "smooth", "windowsLcd", "windows"]
                .into_iter()
                .filter_map(|k| photocraft_engine::commands::find(&format!("type.antiAlias.{k}")).map(|c| (k.to_string(), c.label)))
                .collect();
            if crate::widgets::dropdown(ui, "props-type-aa", &mut cur, &opts, dd_w) {
                run = Some(format!("type.antiAlias.{cur}"));
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (key, tip, on) in [("horizontal", "Horizontal type", !vertical), ("vertical", "Vertical type", vertical)] {
                let clicked = toggle_cell(ui, 28.0, on, tip, |ui, r, col| {
                    let g = egui::Rect::from_center_size(r.center(), egui::vec2(12.0, 12.0));
                    let s = Stroke::new(1.3, col);
                    if key == "horizontal" {
                        ui.painter().line_segment([g.left_top(), g.right_top()], s);
                        ui.painter().line_segment([g.center_top(), g.center_bottom()], s);
                    } else {
                        ui.painter().line_segment([g.left_top(), g.left_bottom()], s);
                        ui.painter().line_segment([g.left_center(), g.right_center()], s);
                    }
                });
                if clicked && !on {
                    run = Some(format!("type.orientation.{key}"));
                }
            }
        });
    });
    ui.add_space(crate::theme::ROW_GAP);
    if let Some(id) = run
        && let Err(e) = app.run(&id, json!({}))
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Cancel the editing session: undo it back to where it started (removes a new layer).
pub fn cancel(app: &mut PhotocraftApp) {
    let Some(ed) = app.ui.text_edit.take() else { return };
    let coalesced = app.session.active().is_some_and(|s| s.coalesce.as_deref() == Some(ed.session.as_str()));
    if coalesced {
        let _ = app.run("edit.undo", json!({}));
    }
}

#[cfg(test)]
#[path = "type_tool_tests.rs"]
mod canvas_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 400, "height": 200})).unwrap();
        app.sync_views();
        app.ui.tool = crate::state::Tool::Type;
        app
    }

    fn layer_text(app: &PhotocraftApp) -> String {
        let ed = app.ui.text_edit.as_ref().unwrap();
        current_text(app, LayerId(ed.layer)).unwrap()
    }

    #[test]
    fn click_creates_placeholder_selected_and_typing_replaces_it() {
        let mut app = app();
        assert!(!pointer_down(&mut app, 50.0, 100.0, false));
        pointer_up(&mut app, [50.0, 100.0], [50.0, 100.0]);
        let ed = app.ui.text_edit.clone().unwrap();
        assert_eq!((ed.anchor, ed.caret), (0, PLACEHOLDER.chars().count()));
        insert(&mut app, "Héllo");
        insert(&mut app, " world");
        assert_eq!(layer_text(&app), "Héllo world");
        // Creating and typing share the session key: one step after the initial snapshot.
        let steps = app.session.active().unwrap().history.entries();
        assert_eq!(steps, ["Open", "New Type Layer"]);
        commit(&mut app);
        let doc = &app.session.active().unwrap().doc;
        assert_eq!(doc.layers.last().unwrap().name, "Héllo world");
        assert!(app.ui.text_edit.is_none());
    }

    #[test]
    fn ime_preedit_is_replaced_by_each_update_and_commit_finalises() {
        let mut app = app();
        pointer_up(&mut app, [50.0, 100.0], [50.0, 100.0]);
        // Each preedit update replaces the previous one (and the placeholder selection first).
        ime_update(&mut app, "に", false);
        assert_eq!(layer_text(&app), "に");
        ime_update(&mut app, "にほん", false);
        ime_update(&mut app, "にほんご", false);
        assert_eq!(layer_text(&app), "にほんご");
        assert_eq!(app.ui.text_edit.as_ref().unwrap().preedit, Some((0, 4)));
        ime_update(&mut app, "日本語", true);
        assert_eq!(layer_text(&app), "日本語");
        let ed = app.ui.text_edit.clone().unwrap();
        assert_eq!((ed.caret, ed.anchor, ed.preedit), (3, 3, None));
        // The next composition starts after the committed text; cancelling removes it again.
        ime_update(&mut app, "あ", false);
        assert_eq!(layer_text(&app), "日本語あ");
        ime_update(&mut app, "", false);
        assert_eq!(layer_text(&app), "日本語");
        assert_eq!(app.ui.text_edit.as_ref().unwrap().preedit, None);
        // A stale preedit range (text shortened by undo) must not panic.
        app.ui.text_edit.as_mut().unwrap().preedit = Some((10, 5));
        ime_update(&mut app, "x", true);
        assert_eq!(layer_text(&app), "日本語x");
    }

    #[test]
    fn empty_new_layer_is_deleted_on_commit() {
        let mut app = app();
        pointer_up(&mut app, [10.0, 50.0], [10.0, 50.0]);
        let n = app.session.active().unwrap().doc.layers.len();
        insert(&mut app, "");
        if let Some(e) = app.ui.text_edit.as_mut() {
            e.anchor = 0;
        }
        insert(&mut app, "");
        commit(&mut app);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), n - 1);
    }

    #[test]
    fn word_boundaries() {
        assert_eq!(word_boundary("hello big world", 0, true), 5);
        assert_eq!(word_boundary("hello big world", 7, false), 6);
        assert_eq!(word_boundary("hello big world", 15, false), 10);
    }
}
