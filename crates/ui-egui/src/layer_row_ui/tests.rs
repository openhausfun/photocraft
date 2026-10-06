//! #144: the name never runs under the right-hand indicators; the fx triangle; #143: Collapse
//! All Groups.

use egui::{Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_doc::LayerContent;
use serde_json::json;

use super::{Indicator, RowRects, layout, recorded};
use crate::PhotocraftApp;

const LONG: &str = "Button / Primary / Hover state — a very long descriptive layer name 背景のテクスチャ that keeps going";

/// Long-named layers with every indicator, nested four groups deep.
fn busy() -> photocraft_engine::Session {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    let mut prev = None;
    for depth in 0..4 {
        let a = s.execute("layer.new.layer", json!({"name": format!("{LONG} {depth}")})).unwrap()["layer"].as_u64().unwrap();
        s.execute("edit.fill", json!({"contents": "color", "color": "#336699"})).unwrap();
        s.execute("layer.layerStyle.dropShadow", json!({"layer": a})).unwrap();
        // #153: the vector mask thumbnail and both link chains take row width too.
        s.execute("layer.vectorMask.revealAll", json!({"layer": a})).unwrap();
        s.execute("layer.setProps", json!({"layer": a, "blend": "Multiply", "locks": {"all": true}})).unwrap();
        s.execute("layer.layerMask.revealAll", json!({"layer": a})).unwrap();
        let b = s.execute("layer.new.layer", json!({"name": format!("{LONG} b{depth}")})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.setProps", json!({"layer": b, "clipped": true})).unwrap();
        s.execute("layer.select", json!({"layer": a})).unwrap();
        s.execute("layer.select", json!({"layer": b, "mode": "toggle"})).unwrap();
        s.execute("layer.linkLayers", json!({})).unwrap();
        if let Some(p) = prev {
            s.execute("layer.select", json!({"layer": p, "mode": "toggle"})).unwrap();
        }
        let g = s.execute("layer.groupLayers", json!({"name": format!("{LONG} group {depth}")})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.layerStyle.dropShadow", json!({"layer": g})).unwrap();
        prev = Some(g);
    }
    s
}

fn harness(session: photocraft_engine::Session, ppp: f32, theme: &str, dock_width: f32) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1440.0, 1000.0)).with_pixels_per_point(ppp).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(session, crate::Services::default())
    });
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new(
        "ui.set",
        json!({"theme": theme, "dockWidth": dock_width, "dock": {"collapsed": ["color", "properties", "history", "navigator"]}}),
    );
    crate::control::handle(h.state_mut(), &ctx, &req);
    h.run_steps(8);
    h
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.intersects(b) && a.intersect(b).area() > 0.0
}

fn check(rows: &[RowRects], what: &str) {
    assert!(rows.len() >= 8, "{what}: rows drawn ({})", rows.len());
    let mut with_fx = 0;
    for r in rows {
        let name = r.name.unwrap_or_else(|| panic!("{what}: layer {} has no room for its name", r.layer));
        assert!(name.right() <= r.row.right(), "{what}: name inside its row");
        for (k, ind) in &r.indicators {
            assert!(!overlaps(name, *ind), "{what}: layer {} name {name:?} overlaps {k:?} {ind:?}", r.layer);
            assert!(ind.right() <= r.row.right() - super::RIGHT_PAD + 0.01, "{what}: {k:?} clear of the scrollbar");
        }
        for (i, (ka, a)) in r.indicators.iter().enumerate() {
            for (kb, b) in &r.indicators[i + 1..] {
                assert!(!overlaps(*a, *b), "{what}: {ka:?} overlaps {kb:?}");
            }
        }
        with_fx += r.indicators.iter().any(|(k, _)| *k == Indicator::Fx) as usize;
    }
    assert!(with_fx >= 4, "{what}: fx badges drawn ({with_fx})");
}

#[test]
fn layout_gives_the_name_what_the_indicators_leave() {
    let row = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 32.0));
    let items = [(Indicator::Lock, 14.0), (Indicator::FxTriangle, 10.0), (Indicator::Fx, 12.0), (Indicator::Link, 14.0), (Indicator::Blend, 40.0)];
    let (rects, right) = layout(row, 100.0, &items);
    assert_eq!(rects.len(), 5);
    assert!(rects.iter().all(|(_, r)| r.left() > right && r.right() <= 300.0 - super::RIGHT_PAD));
    // Narrow: the optional blend label goes first; the rest stay.
    let (rects, right) = layout(row, 200.0, &items);
    assert_eq!(rects.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [Indicator::Lock, Indicator::FxTriangle, Indicator::Fx, Indicator::Link]);
    assert!(right < rects[3].1.left());
}

#[test]
fn long_names_never_run_under_the_indicators() {
    for ppp in [1.0, 1.5, 2.0] {
        for width in [250.0, 290.0, 520.0] {
            let h = harness(busy(), ppp, "promedium", width);
            check(&recorded(&h.ctx), &format!("@{ppp}x {width}pt"));
        }
    }
    for theme in ["pro", "studio", "studiolight", "classic"] {
        let h = harness(busy(), 1.0, theme, 250.0);
        check(&recorded(&h.ctx), theme);
    }
}

fn click(h: &mut Harness<'_, PhotocraftApp>, at: Pos2) {
    h.hover_at(at);
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

#[test]
fn the_fx_triangle_hides_and_shows_the_effects_rows() {
    let mut h = harness(busy(), 1.0, "promedium", 290.0);
    let top = h.state().session.active().unwrap().doc.layers.last().unwrap().clone();
    let rows = |h: &Harness<'_, PhotocraftApp>| recorded(&h.ctx).iter().map(|r| r.row.top()).collect::<Vec<_>>();
    let before = rows(&h);
    let label = format!("Collapse effects {}", top.name);
    let p = h.get_by_label(&label).rect().center();
    let active = h.state().session.active().unwrap().active_layer;
    click(&mut h, p);
    assert!(h.state().session.active().unwrap().fx_collapsed.contains(&top.id));
    assert_eq!(h.state().session.active().unwrap().active_layer, active, "the triangle doesn't select");
    // Its Effects / Drop Shadow sub-rows are gone, so the next layer row moved up.
    let after = rows(&h);
    assert!(after[1] < before[1], "rows moved up: {:?} -> {:?}", &before[..2], &after[..2]);
    let p = h.get_by_label(&format!("Expand effects {}", top.name)).rect().center();
    click(&mut h, p);
    assert!(h.state().session.active().unwrap().fx_collapsed.is_empty());
}

fn groups_open(s: &photocraft_engine::Session) -> Vec<bool> {
    s.active().unwrap().doc.walk().into_iter().filter_map(|(_, _, l)| if let LayerContent::Group(g) = &l.content { Some(g.expanded) } else { None }).collect()
}

#[test]
fn collapse_all_groups_from_the_panel_menu_and_it_is_saved() {
    let mut h = harness(busy(), 1.0, "promedium", 290.0);
    assert!(groups_open(&h.state().session).iter().all(|o| *o));
    let menu = crate::dock::last_rects(&h.ctx).into_iter().find(|(g, _)| *g == crate::dock::Group::Layers).expect("layers group").1;
    // The hamburger sits at the right end of the group's tab strip.
    click(&mut h, pos2(menu.right() - 14.0, menu.top() + 14.0));
    let item = h.get_by_label("Collapse All Groups").rect().center();
    click(&mut h, item);
    let open = groups_open(&h.state().session);
    assert_eq!(open.len(), 4);
    assert!(open.iter().all(|o| !o), "every group closed: {open:?}");
    // Collapsed state is document data: it survives a PSD and a .pcraft round trip.
    let doc = (*h.state().session.active().unwrap().doc).clone();
    for name in ["c.psd", "c.pcraft"] {
        let bytes = photocraft_io::export(&doc, name, &Default::default()).unwrap().bytes;
        let back = photocraft_io::import(name, &bytes).unwrap().document;
        let mut s = photocraft_engine::Session::new();
        s.add_document(back, None);
        assert!(groups_open(&s).iter().all(|o| !o), "{name}: groups stay closed");
    }
}
