//! Type tool through the real canvas (#123, #124): clicks and drags at known glyph positions
//! place the caret and selection on the glyph under the pointer at any zoom, HiDPI scale, pan,
//! view flip and layer transform; size edits apply at layer and selection scope, for native and
//! PSD-round-tripped text, as one history step per drag.

use egui::{Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_doc::{LayerContent, LayerId, TextLayer};
use photocraft_geom::{Affine, Point};
use serde_json::json;

use super::{layout, text_layer};
use crate::PhotocraftApp;
use crate::canvas::ViewXform;

fn harness(ppp: f32, app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_pixels_per_point(ppp).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::shortcuts::handle(app, &ctx);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    h
}

fn new_app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1600, "height": 900})).unwrap();
    app.sync_views();
    app.ui.extras.rulers = false;
    app.ui.tool = crate::state::Tool::Type;
    app
}

fn xf(app: &PhotocraftApp) -> ViewXform {
    let v = &app.ui.views[0];
    ViewXform { rect: crate::rulers::content_rect(app, app.last_canvas_rect), zoom: v.zoom, center: v.center, flip: app.ui.view.flip_horizontal }
}

/// Screen position of a text-space point of layer `id`.
fn screen(h: &mut Harness<'static, PhotocraftApp>, id: LayerId, x: f32, y: f32) -> Pos2 {
    let app = h.state_mut();
    let (_, aff, _) = layout(app, id).unwrap();
    let p = aff.apply(Point::new(f64::from(x), f64::from(y)));
    xf(app).to_screen(p.x as f32, p.y as f32)
}

/// A point inside glyph `i` (character index): `f` of the way along its advance, in the upper
/// half of its line.
fn glyph(h: &mut Harness<'static, PhotocraftApp>, id: LayerId, i: usize, f: f32) -> Pos2 {
    let (l, _, text) = layout(h.state_mut(), id).unwrap();
    let b = text.char_indices().nth(i).unwrap().0;
    let c = l.clusters.iter().find(|c| c.range.start == b).unwrap().clone();
    let ln = &l.lines[c.line];
    screen(h, id, c.x + c.advance * f, ln.baseline - ln.ascent * 0.35)
}

fn press(h: &mut Harness<'static, PhotocraftApp>, p: Pos2, down: bool) {
    h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: down, modifiers: Modifiers::NONE });
    h.run_steps(1);
}

fn click(h: &mut Harness<'static, PhotocraftApp>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    press(h, p, true);
    press(h, p, false);
    h.run_steps(1);
}

fn drag(h: &mut Harness<'static, PhotocraftApp>, a: Pos2, b: Pos2) {
    h.hover_at(a);
    h.run_steps(1);
    press(h, a, true);
    for k in 1..=6 {
        h.hover_at(a + (b - a) * (k as f32 / 6.0));
        h.run_steps(1);
    }
    press(h, b, false);
    h.run_steps(1);
}

fn selection(h: &Harness<'static, PhotocraftApp>) -> (usize, usize) {
    let Some(e) = h.state().ui.text_edit.clone() else { return (usize::MAX, usize::MAX) };
    (e.anchor, e.caret)
}

/// Every combination the issue lists that this canvas has: zoom 25–200 %, HiDPI 1×/2×, a panned
/// view, View › Flip Horizontal, and point / paragraph text with identity or rotated+scaled layer
/// transforms. Clicks land before/after the glyph under the pointer; drags select from the glyph
/// pressed (not where egui recognised the drag) to the glyph released over.
#[test]
fn clicks_and_drags_land_on_the_glyph_under_the_pointer() {
    let rotated = Affine { m: [1.4 * 0.94, 1.4 * 0.342, -1.4 * 0.342, 1.4 * 0.94, 500.0, 420.0] };
    for ppp in [1.0, 2.0] {
        for zoom in [0.25f32, 0.5, 1.0, 2.0] {
            for (flip, pan) in [(false, [0.0, 0.0]), (true, [37.0, -21.0])] {
                for (shape, tf) in [("point", None), ("box", None), ("point", Some(rotated)), ("box", Some(rotated))] {
                    let mut app = new_app();
                    let size = 60.0 / zoom;
                    let mut p = json!({"text": "HOHOHO HOHO", "size": size, "x": 300, "y": 420});
                    if shape == "box" {
                        p["box"] = json!([250.0, 250.0, size * 6.0, size * 4.0]);
                    }
                    let id = LayerId(app.run("type.create", p).unwrap()["layer"].as_u64().unwrap());
                    if let Some(a) = tf {
                        app.run("type.edit", json!({"layer": id.0, "transform": a.m})).unwrap();
                    }
                    app.ui.view.flip_horizontal = flip;
                    let mut h = harness(ppp, app);
                    // Centre the view on the text's middle, then pan.
                    let (l, aff, _) = layout(h.state_mut(), id).unwrap();
                    let b = l.bounds().unwrap();
                    let m = aff.apply(Point::new(f64::from(b[0] + b[2]) / 2.0, f64::from(b[1] + b[3]) / 2.0));
                    let v = &mut h.state_mut().ui.views[0];
                    (v.zoom, v.center, v.fit_pending) = (zoom, [m.x as f32 + pan[0], m.y as f32 + pan[1]], false);
                    h.run_steps(2);
                    let ctx = format!("ppp {ppp} zoom {zoom} flip {flip} {shape} transformed {}", tf.is_some());
                    let rect = xf(h.state()).rect;
                    for (i, f) in [(0, 0.2), (2, 0.8), (4, 0.3), (8, 0.7)] {
                        let p = glyph(&mut h, id, i, f);
                        assert!(rect.contains(p), "{ctx}: glyph {i} off screen at {p:?}");
                    }
                    // Click into the layer: caret before glyph 2 (left part), then after glyph 4.
                    let p = glyph(&mut h, id, 2, 0.2);
                    click(&mut h, p);
                    assert_eq!(selection(&h), (2, 2), "{ctx}: click on the left of glyph 2");
                    let p = glyph(&mut h, id, 4, 0.8);
                    click(&mut h, p);
                    assert_eq!(selection(&h), (5, 5), "{ctx}: click on the right of glyph 4");
                    // Drag from glyph 1 (left part) to glyph 8 (right part): characters 1..9.
                    let (a, b) = (glyph(&mut h, id, 1, 0.25), glyph(&mut h, id, 8, 0.75));
                    drag(&mut h, a, b);
                    assert_eq!(selection(&h), (1, 9), "{ctx}: drag selection");
                    // And backwards, starting inside the egui drag threshold of a glyph edge.
                    let (a, b) = (glyph(&mut h, id, 5, 0.85), glyph(&mut h, id, 3, 0.15));
                    drag(&mut h, a, b);
                    assert_eq!(selection(&h), (6, 3), "{ctx}: backwards drag selection");
                }
            }
        }
    }
}

fn text(app: &PhotocraftApp, id: LayerId) -> TextLayer {
    text_layer(&app.session.active().unwrap().doc, id).unwrap().clone()
}

/// A PSD type layer keeps Photoshop's pixels until it is edited. Clicking into it re-renders it
/// with our engine first (so the caret sits on what is shown), inside the edit session's single
/// history step; Cancel brings Photoshop's pixels back.
#[test]
fn editing_psd_type_shows_our_layout_and_cancel_restores_it() {
    let mut app = new_app();
    let id = LayerId(app.run("type.create", json!({"text": "HOHOHO", "size": 120, "x": 300, "y": 420})).unwrap()["layer"].as_u64().unwrap());
    // Stand-in for Photoshop's rendering: the same text drawn somewhere else.
    let moved = {
        let mut t = text(&app, id);
        t.transform = Affine::translate(330.0, 380.0);
        photocraft_text::shared().lock().unwrap().render(&t, 72.0, photocraft_color::PixelFormat::RGBA8).1.surface
    };
    let st = app.session.active_mut().unwrap();
    let mut doc = (*st.doc).clone();
    if let Some(LayerContent::Text(t)) = doc.layer_mut(id).map(|l| &mut l.content) {
        t.cache = Some(moved.clone());
    }
    st.doc = std::sync::Arc::new(doc);
    st.revision += 1;
    let steps = app.session.active().unwrap().history.entries().len();
    let mut h = harness(1.0, app);
    let p = glyph(&mut h, id, 2, 0.2);
    click(&mut h, p);
    assert_eq!(selection(&h), (2, 2));
    let ours = text(h.state(), id).cache.unwrap().content_bounds();
    assert_ne!(ours, moved.content_bounds(), "re-rendered where the caret is");
    assert_eq!(h.state().session.active().unwrap().history.entries().len(), steps + 1);
    super::insert(h.state_mut(), "x");
    assert_eq!(h.state().session.active().unwrap().history.entries().len(), steps + 1, "one step for the session");
    super::cancel(h.state_mut());
    assert_eq!(text(h.state(), id).cache.unwrap().content_bounds(), moved.content_bounds());
    assert_eq!(text(h.state(), id).text, "HOHOHO");
    // A native layer shows our layout already: clicking into it records nothing.
    let mut app = new_app();
    let id = LayerId(app.run("type.create", json!({"text": "HOHOHO", "size": 120, "x": 300, "y": 420})).unwrap()["layer"].as_u64().unwrap());
    let steps = app.session.active().unwrap().history.entries().len();
    let mut h = harness(1.0, app);
    let p = glyph(&mut h, id, 3, 0.2);
    click(&mut h, p);
    assert_eq!(selection(&h), (3, 3));
    assert_eq!(h.state().session.active().unwrap().history.entries().len(), steps);
}

fn size_at(app: &PhotocraftApp, id: LayerId, ci: usize) -> f32 {
    let t = text(app, id);
    let b = t.text.char_indices().nth(ci).map_or(t.text.len(), |(b, _)| b);
    let mut at = 0;
    for r in t.char_runs() {
        if b < at + r.len {
            return r.style.size_pt;
        }
        at += r.len;
    }
    0.0
}

/// Font size from the options bar / Properties: the whole layer when no characters are selected,
/// only the selection when editing with one, as the layer appears (transform scale included), and
/// for type that went through a PSD file.
#[test]
fn size_applies_at_layer_and_selection_scope() {
    for psd in [false, true] {
        let mut app = new_app();
        let id = LayerId(app.run("type.create", json!({"text": "Hello world", "size": 20, "x": 300, "y": 420})).unwrap()["layer"].as_u64().unwrap());
        app.run("type.edit", json!({"layer": id.0, "transform": [2.0, 0.0, 0.0, 2.0, 300.0, 420.0]})).unwrap();
        let id = if psd {
            let doc = (*app.session.active().unwrap().doc).clone();
            let out = photocraft_io::export(&doc, "t.psd", &Default::default()).unwrap();
            let back = photocraft_io::import("t.psd", &out.bytes).unwrap().document;
            app.session.add_document(back, None);
            app.sync_views();
            let d = &app.session.active().unwrap().doc;
            let id = d.walk().into_iter().find(|(_, _, l)| matches!(l.content, LayerContent::Text(_))).map(|(_, _, l)| l.id).unwrap();
            let _ = app.session.select_layer(id);
            id
        } else {
            id
        };
        let ctx = egui::Context::default();
        assert!((super::shown_scale(&app) - 2.0).abs() < 1e-4, "psd {psd}");
        let w0 = text(&app, id).cache.unwrap().content_bounds().width();
        // Layer scope: 60 pt as shown = 30 pt in the layer's own space.
        let k = super::shown_scale(&app);
        super::apply(&mut app, &ctx, json!({"size": 60.0 / k}));
        assert_eq!((size_at(&app, id, 0), size_at(&app, id, 10)), (30.0, 30.0), "psd {psd}");
        let w1 = text(&app, id).cache.unwrap().content_bounds().width();
        assert!(w1 as f32 > w0 as f32 * 1.4, "psd {psd}: the pixels grow ({w0} → {w1})");
        // Selection scope: "world" only.
        app.ui.text_edit =
            Some(crate::state::TextEdit { layer: id.0, caret: 11, anchor: 6, session: "s".into(), created: false, dragging: false, preedit: None });
        super::apply(&mut app, &ctx, json!({"size": 10.0}));
        assert_eq!((size_at(&app, id, 0), size_at(&app, id, 5), size_at(&app, id, 6), size_at(&app, id, 10)), (30.0, 30.0, 10.0, 10.0), "psd {psd}");
    }
}

/// Scrubbing the options bar's size field previews live (the size changes every frame) and is
/// one undo step per drag; each step only damages the type layer's area, not the whole canvas.
#[test]
fn size_drag_is_live_and_one_history_step_per_drag() {
    let mut app = new_app();
    let id = LayerId(app.run("type.create", json!({"text": "Hello", "size": 20, "x": 300, "y": 420})).unwrap()["layer"].as_u64().unwrap());
    let mut h = Harness::builder().with_size(vec2(900.0, 60.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            ui.horizontal(|ui| super::options_bar(app, ui));
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(3);
    let steps = h.state().session.active().unwrap().history.entries().len();
    let mut last = 20.0;
    for d in 0..2 {
        let field = h.get_all_by_role(egui::accesskit::Role::SpinButton).next().expect("size field").rect().center();
        h.hover_at(field);
        h.run_steps(1);
        press(&mut h, field, true);
        for k in 1..=5 {
            h.hover_at(field + vec2(8.0 * k as f32, 0.0));
            h.run_steps(1);
            let now = size_at(h.state(), id, 0);
            assert!(now > last, "drag {d} step {k}: live size {last} → {now}");
            last = now;
            let dmg = h.state().session.active().unwrap().last_damage.expect("damage rect, not a full refresh");
            assert!(!dmg.is_empty() && dmg.width() * dmg.height() <= 512 * 512, "{dmg:?}");
        }
        press(&mut h, field + vec2(40.0, 0.0), false);
        h.run_steps(2);
        assert_eq!(h.state().session.active().unwrap().history.entries().len(), steps + d + 1, "one step per drag");
    }
    assert!(h.state_mut().session.undo());
    assert!(h.state_mut().session.undo());
    assert_eq!(size_at(h.state(), id, 0), 20.0);
}

/// A point inside glyph `i` of vertical type: `f` of the way down its advance, right of the
/// column's centre line.
fn vglyph(h: &mut Harness<'static, PhotocraftApp>, id: LayerId, i: usize, f: f32) -> Pos2 {
    let (l, _, text) = layout(h.state_mut(), id).unwrap();
    assert!(l.vertical);
    let b = text.char_indices().nth(i).unwrap().0;
    let c = l.clusters.iter().find(|c| c.range.start == b).unwrap().clone();
    let ln = &l.lines[c.line];
    let (x, y) = l.to_text(c.x + c.advance * f, ln.baseline - ln.ascent * 0.35);
    screen(h, id, x, y)
}

fn key(h: &mut Harness<'static, PhotocraftApp>, k: egui::Key) {
    h.event(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(1);
}

/// Vertical type (#199): clicks and drags land on the glyph under the pointer down the column,
/// and the arrow keys follow the vertical flow (↓ next character, ← next column).
#[test]
fn vertical_type_caret_selection_and_arrows_follow_the_columns() {
    let rotated = Affine { m: [0.94, 0.342, -0.342, 0.94, 900.0, 200.0] };
    for ppp in [1.0, 2.0] {
        for tf in [None, Some(rotated)] {
            let mut app = new_app();
            let id = LayerId(app.run("type.create", json!({"text": "HOHOHO\nHOHO", "size": 60, "x": 800, "y": 120})).unwrap()["layer"].as_u64().unwrap());
            app.run("type.orientation.vertical", json!({"layer": id.0})).unwrap();
            // Snapping would pull the pointer onto the layer's edges and centre lines.
            app.ui.extras.snap = false;
            if let Some(a) = tf {
                app.run("type.edit", json!({"layer": id.0, "transform": a.m})).unwrap();
            }
            let mut h = harness(ppp, app);
            let (l, aff, _) = layout(h.state_mut(), id).unwrap();
            let b = l.bounds().unwrap();
            assert!(b[3] - b[1] > b[2] - b[0], "vertical bounds {b:?}");
            let m = aff.apply(Point::new(f64::from(b[0] + b[2]) / 2.0, f64::from(b[1] + b[3]) / 2.0));
            let v = &mut h.state_mut().ui.views[0];
            (v.zoom, v.center, v.fit_pending) = (0.5, [m.x as f32, m.y as f32], false);
            h.run_steps(2);
            let ctx = format!("ppp {ppp} transformed {}", tf.is_some());
            // Upper part of glyph 2 → caret before it; lower part of glyph 4 → after it.
            let p = vglyph(&mut h, id, 2, 0.2);
            click(&mut h, p);
            assert_eq!(selection(&h), (2, 2), "{ctx}");
            let p = vglyph(&mut h, id, 4, 0.8);
            click(&mut h, p);
            assert_eq!(selection(&h), (5, 5), "{ctx}");
            // Drag down the column from glyph 1 into the second column's glyph 2 (char 9).
            let (a, b) = (vglyph(&mut h, id, 1, 0.25), vglyph(&mut h, id, 9, 0.75));
            drag(&mut h, a, b);
            assert_eq!(selection(&h), (1, 10), "{ctx}: drag");
            // Arrows: ↓ next char, ↑ previous, ← next column (same position), → back.
            let p = vglyph(&mut h, id, 1, 0.2);
            click(&mut h, p);
            assert_eq!(selection(&h), (1, 1), "{ctx}");
            key(&mut h, egui::Key::ArrowDown);
            assert_eq!(selection(&h).1, 2, "{ctx}: ↓");
            key(&mut h, egui::Key::ArrowUp);
            assert_eq!(selection(&h).1, 1, "{ctx}: ↑");
            key(&mut h, egui::Key::ArrowLeft);
            assert_eq!(selection(&h).1, 8, "{ctx}: ← moves to the next column");
            key(&mut h, egui::Key::ArrowRight);
            assert_eq!(selection(&h).1, 1, "{ctx}: → moves back");
        }
    }
    assert_eq!(super::flow_key(egui::Key::ArrowUp, false), egui::Key::ArrowUp);
}
