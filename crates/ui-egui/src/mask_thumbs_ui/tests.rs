//! #153: mask thumbnails, link chains and the red X in Layers rows; #154: the layer mask in
//! Channels, the layer's path in Paths, aspect-correct channel thumbnails, the Paths footer.

use egui::{Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use super::{MaskKind, THUMB_VECTOR, recorded, vector_thumb_image};
use crate::PhotocraftApp;

/// A 200×100 document: "Masked" (pixel + vector mask) and a "Badge" shape layer.
fn session() -> (photocraft_engine::Session, u64, u64) {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 200, "height": 100})).unwrap();
    let masked = s.execute("layer.new.layer", json!({"name": "Masked"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("edit.fill", json!({"contents": "color", "color": "#336699"})).unwrap();
    s.execute("layer.layerMask.revealAll", json!({"layer": masked})).unwrap();
    let path = json!({"subpaths": [{"knots": [[20, 10], [120, 10], [120, 90], [20, 90]]}]});
    s.execute("layer.vectorMask.add", json!({"layer": masked, "path": path})).unwrap();
    let shape =
        s.execute("shape.create", json!({"kind": "rect", "rect": [140, 20, 40, 40], "fill": "#cc3333", "name": "Badge"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.select", json!({"layer": masked})).unwrap();
    (s, masked, shape)
}

fn harness(session: photocraft_engine::Session, tab: usize, ppp: f32, dock_width: f32) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1440.0, 1000.0)).with_pixels_per_point(ppp).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(session, crate::Services::default())
    });
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new(
        "ui.set",
        json!({"theme": "promedium", "dockWidth": dock_width, "dockTabs": {"layers": tab}, "dock": {"collapsed": ["color", "properties", "history", "navigator"]}}),
    );
    crate::control::handle(h.state_mut(), &ctx, &req);
    h.run_steps(8);
    h
}

fn click_with(h: &mut Harness<'_, PhotocraftApp>, at: Pos2, modifiers: Modifiers) {
    h.hover_at(at);
    h.event(egui::Event::ModifiersChanged(modifiers));
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers });
    h.run_steps(1);
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
}

fn layer(h: &Harness<'_, PhotocraftApp>, id: u64) -> photocraft_doc::Layer {
    let st = h.state().session.active().unwrap();
    st.doc.layer(photocraft_doc::LayerId(id)).unwrap().clone()
}

#[test]
fn rows_show_both_mask_thumbnails_with_chains_and_cache_the_vector_one() {
    for ppp in [1.0, 2.0] {
        let (s, masked, _) = session();
        let h = harness(s, 0, ppp, 290.0);
        let (thumbs, chains) = recorded(&h.ctx, masked).expect("mask thumbnails drawn");
        assert_eq!(thumbs.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [MaskKind::Pixel, MaskKind::Vector]);
        assert_eq!(chains.len(), 2);
        // layer thumbnail | chain | pixel mask | chain | vector mask, without overlaps.
        for i in 0..2 {
            assert!(chains[i].1.right() <= thumbs[i].1.left() + 0.01, "@{ppp}x chain {i} before its thumbnail");
        }
        assert!(thumbs[0].1.right() <= chains[1].1.left() + 0.01);
        let row = crate::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == masked).unwrap();
        assert!(row.name.unwrap().left() >= thumbs[1].1.right(), "@{ppp}x the name starts after the masks");
        assert!(h.state().thumbs.contains_key(&(photocraft_doc::LayerId(masked), THUMB_VECTOR)), "vector thumbnail cached");
    }
}

#[test]
fn deleting_the_vector_mask_prunes_its_thumbnail() {
    let (s, masked, _) = session();
    let mut h = harness(s, 0, 1.0, 290.0);
    let key = (photocraft_doc::LayerId(masked), THUMB_VECTOR);
    assert!(h.state().thumbs.contains_key(&key));
    h.state_mut().run("layer.vectorMask.delete", json!({"layer": masked})).unwrap();
    h.run_steps(3);
    assert!(!h.state().thumbs.contains_key(&key));
}

#[test]
fn the_chain_toggles_linking_and_it_survives_psd() {
    let (s, masked, _) = session();
    let mut h = harness(s, 0, 1.0, 290.0);
    let p = h.get_by_label("Unlink layer mask Masked").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert!(!layer(&h, masked).mask.unwrap().linked);
    let p = h.get_by_label("Unlink vector mask Masked").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert!(!layer(&h, masked).vector_mask.unwrap().linked);
    let doc = (*h.state().session.active().unwrap().doc).clone();
    let bytes = photocraft_io::export(&doc, "m.psd", &Default::default()).unwrap().bytes;
    let back = photocraft_io::import("m.psd", &bytes).unwrap().document;
    let l = back.walk().into_iter().map(|(_, _, l)| l.clone()).find(|l| l.name == "Masked").unwrap();
    assert!(!l.mask.unwrap().linked && !l.vector_mask.unwrap().linked, "unlinked state round-trips PSD");
    // Clicking the (now empty) chain slot links again.
    let p = h.get_by_label("Link layer mask Masked").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert!(layer(&h, masked).mask.unwrap().linked);
}

#[test]
fn shift_click_disables_a_mask() {
    let (s, masked, _) = session();
    let mut h = harness(s, 0, 1.0, 290.0);
    for kind in [MaskKind::Pixel, MaskKind::Vector] {
        let r = recorded(&h.ctx, masked).unwrap().0.into_iter().find(|(k, _)| *k == kind).unwrap().1;
        click_with(&mut h, r.center(), Modifiers::SHIFT);
        h.run_steps(2);
    }
    let l = layer(&h, masked);
    assert!(!l.mask.unwrap().enabled && !l.vector_mask.unwrap().enabled);
}

#[test]
fn vector_thumbnail_is_white_inside_grey_outside_at_the_document_aspect() {
    let (s, masked, _) = session();
    let doc = s.active().unwrap().doc.clone();
    let vm = doc.layer(photocraft_doc::LayerId(masked)).unwrap().vector_mask.clone().unwrap();
    let img = vector_thumb_image(&doc, &vm, 64);
    let at = |x: usize, y: usize| img.pixels[y * 64 + x];
    // 200×100 doc in 64 px: 0.32 scale, rows 16..48. The mask covers x 20..120, y 10..90.
    assert_eq!(at(20, 32), egui::Color32::from_gray(255), "inside reveals");
    assert_eq!(at(55, 32), egui::Color32::from_gray(128), "outside hides");
    assert_eq!(at(20, 4), egui::Color32::TRANSPARENT, "letterbox");
}

#[test]
fn channels_list_the_layer_mask_and_keep_the_aspect() {
    let (s, masked, _) = session();
    let mut h = harness(s, 1, 1.0, 290.0);
    let rows = crate::channels_panel::recorded(&h.ctx);
    let names: Vec<_> = rows.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["RGB", "Red", "Green", "Blue", "Masked Mask"], "the mask is a temporary row below the colour channels");
    for (n, r) in &rows {
        assert!((r.width() / r.height() - 2.0).abs() < 0.05, "{n}: 2:1 thumbnail, got {r:?}");
    }
    assert!(!h.state().ui.mask_target);
    let p = h.get_by_label("Masked Mask").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert!(h.state().ui.mask_target, "clicking the mask row targets the mask");
    assert_eq!(crate::canvas::paint_target(h.state()), json!("mask"));
    h.run_steps(2);
    let p = h.get_by_label("RGB").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert!(!h.state().ui.mask_target, "the composite targets the pixels again");
    // A layer without a mask has no mask row.
    h.state_mut().run("layer.layerMask.delete", json!({"layer": masked})).unwrap();
    h.run_steps(3);
    assert!(crate::channels_panel::recorded(&h.ctx).iter().all(|(n, _)| !n.ends_with("Mask")));
}

#[test]
fn paths_list_the_layer_path_above_a_bottom_footer() {
    let (s, masked, shape) = session();
    let mut h = harness(s, 2, 1.0, 290.0);
    let row = h.get_by_label("Masked Vector Mask").rect();
    let footer = crate::vector_ui::paths_footer(&h.ctx).expect("footer drawn");
    assert!(footer.top() > row.bottom(), "buttons below the rows");
    let group = crate::dock::last_rects(&h.ctx).into_iter().find(|(g, _)| *g == crate::dock::Group::Layers).unwrap().1;
    assert!(group.bottom() - footer.bottom() < 16.0, "footer at the panel bottom: footer {footer:?}, group {group:?}");
    assert!(footer.bottom() - row.bottom() > 100.0, "the footer is pinned, not right below the rows");
    h.state_mut().run("layer.select", json!({"layer": shape})).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("Badge Shape Path").is_some());
    assert!(h.query_by_label("Masked Vector Mask").is_none(), "only the selected layer's path");
    // Selecting the temporary row targets the layer's path: Load as selection uses it.
    let p = h.get_by_label("Badge Shape Path").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert_eq!(h.state().ui.selected_path.as_deref(), Some("layer"));
    // Footer: fill, stroke, load as selection, … (24 pt buttons, 2 pt apart).
    let footer = crate::vector_ui::paths_footer(&h.ctx).unwrap();
    let p = pos2(footer.left() + 2.0 * 26.0 + 12.0, footer.center().y);
    click_with(&mut h, p, Modifiers::NONE);
    let sel = h.state().session.active().unwrap().doc.selection.clone().expect("selection from the shape path");
    let b = sel.content_bounds();
    assert!(b.x0.abs_diff(140) <= 1 && b.width().abs_diff(40) <= 1, "{b:?}");
    let _ = masked;
}

#[test]
fn panels_never_panic_on_odd_documents() {
    // Rows render for a 1×1 and a very wide document, and the fitted thumbnails stay finite.
    for (w, hgt) in [(1, 1), (2000, 3)] {
        for tab in 0..3 {
            let mut s = photocraft_engine::Session::new();
            s.execute("file.new", json!({"width": w, "height": hgt})).unwrap();
            s.execute("layer.new.layer", json!({})).unwrap();
            s.execute("layer.layerMask.hideAll", json!({})).unwrap();
            s.execute("layer.vectorMask.revealAll", json!({})).unwrap();
            let _ = harness(s, tab, 1.0, 250.0);
        }
    }
    let (r, uv) = crate::channels_panel::fit_thumb(Rect::from_min_size(pos2(0.0, 0.0), vec2(28.0, 28.0)), 0, 0);
    assert!(r.is_finite() && uv.is_finite() && r.width() > 0.0);
}

// ---------------------------------------------------------------------------------------------
// #196: ⌥-click mask view, ⌘-click vector mask to select, vector-mask target brackets.

/// The session above with a black dot painted into the layer mask at (50, 50).
fn dotted() -> (photocraft_engine::Session, u64, u64) {
    let (mut s, masked, shape) = session();
    s.execute("paint.stroke", json!({"points": [[50, 50]], "size": 20, "hardness": 1.0, "color": "#000000", "target": "mask"})).unwrap();
    (s, masked, shape)
}

fn mask_rect(h: &Harness<'_, PhotocraftApp>, id: u64, kind: MaskKind) -> Rect {
    recorded(&h.ctx, id).unwrap().0.into_iter().find(|(k, _)| *k == kind).unwrap().1
}

/// The layer thumbnail sits left of the first chain (see `paint`).
fn layer_thumb_center(h: &Harness<'_, PhotocraftApp>, id: u64) -> Pos2 {
    let first = recorded(&h.ctx, id).unwrap().0[0].1;
    pos2(first.left() - 4.0 - super::CHAIN_W - first.width() / 2.0, first.center().y)
}

fn mask_view(h: &Harness<'_, PhotocraftApp>) -> serde_json::Value {
    photocraft_engine::inspect::document(h.state().session.active().unwrap())["layerMaskView"].clone()
}

fn canvas_px(h: &Harness<'_, PhotocraftApp>) -> Option<Vec<egui::Color32>> {
    let st = h.state().session.active().unwrap();
    crate::channel_view::render(&st.doc, &st.channel_view, photocraft_geom::Rect::new(0, 0, 200, 100), 1, false)
}

fn near(a: Rect, b: Rect) -> bool {
    (a.min - b.min).length() < 0.6 && (a.max - b.max).length() < 0.6
}

#[test]
fn alt_click_views_the_mask_in_gray_and_the_layer_thumbnail_returns() {
    for ppp in [1.0, 2.0] {
        let (s, masked, _) = dotted();
        let mut h = harness(s, 0, ppp, 290.0);
        let doc = h.state().session.active().unwrap().doc.clone();
        let r = mask_rect(&h, masked, MaskKind::Pixel);
        click_with(&mut h, r.center(), Modifiers::ALT);
        assert_eq!(mask_view(&h), json!({"layer": masked, "mode": "gray"}), "@{ppp}x");
        assert!(h.state().ui.mask_target, "viewing the mask targets it");
        assert_eq!(crate::canvas::paint_target(h.state()), json!("mask"), "painting paints the mask");
        let px = canvas_px(&h).expect("the canvas shows the mask");
        assert_eq!(px[50 * 200 + 50], egui::Color32::BLACK, "hidden reads black");
        assert_eq!(px[10 * 200 + 150], egui::Color32::WHITE, "revealed reads white");
        assert!(near(super::brackets(&h.ctx).unwrap(), r.expand(3.0)), "brackets on the mask");
        // ⌥-click again: back to the composite.
        click_with(&mut h, r.center(), Modifiers::ALT);
        assert_eq!(mask_view(&h), serde_json::Value::Null);
        assert!(canvas_px(&h).is_none());
        // Into mask view again, then out by clicking the layer thumbnail.
        click_with(&mut h, r.center(), Modifiers::ALT);
        assert_eq!(mask_view(&h)["mode"], "gray");
        let p = layer_thumb_center(&h, masked);
        click_with(&mut h, p, Modifiers::NONE);
        assert_eq!(mask_view(&h), serde_json::Value::Null, "@{ppp}x the layer thumbnail returns");
        assert!(!h.state().ui.mask_target, "and targets the pixels");
        assert!(std::sync::Arc::ptr_eq(&doc, &h.state().session.active().unwrap().doc), "viewing never touched the document");
    }
}

#[test]
fn shift_alt_click_shows_a_rubylith_and_the_channels_eye_toggles_it() {
    let (s, masked, _) = dotted();
    let mut h = harness(s, 0, 1.0, 290.0);
    let r = mask_rect(&h, masked, MaskKind::Pixel);
    click_with(&mut h, r.center(), Modifiers::SHIFT | Modifiers::ALT);
    assert_eq!(mask_view(&h), json!({"layer": masked, "mode": "overlay"}));
    let px = canvas_px(&h).unwrap();
    assert_eq!(px[50 * 200 + 50], egui::Color32::from_rgba_premultiplied(128, 0, 0, 128), "50% red over hidden areas");
    assert_eq!(px[10 * 200 + 150], egui::Color32::TRANSPARENT, "the composite shows through elsewhere");
    assert!(layer(&h, masked).mask.unwrap().enabled, "⇧⌥ doesn't disable the mask");
    // Channels: the mask row's eye shows and hides the overlay.
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"dockTabs": {"layers": 1}}));
    crate::control::handle(h.state_mut(), &ctx, &req);
    h.run_steps(4);
    let p = h.get_by_label("Visibility Masked Mask").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert_eq!(mask_view(&h), serde_json::Value::Null, "the eye hides the overlay");
    let p = h.get_by_label("Visibility Masked Mask").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert_eq!(mask_view(&h)["mode"], "overlay", "and shows it again");
    // In gray view the composite's eye brings the composite back under the overlay.
    h.state_mut().run("view.layerMask", json!({"mode": "gray"})).unwrap();
    h.run_steps(3);
    let p = h.get_by_label("Visibility RGB").rect().center();
    click_with(&mut h, p, Modifiers::NONE);
    assert_eq!(mask_view(&h)["mode"], "overlay");
}

#[test]
fn command_click_a_vector_mask_loads_its_path_as_a_selection() {
    let (s, masked, _) = session();
    let mut h = harness(s, 0, 1.0, 290.0);
    let r = mask_rect(&h, masked, MaskKind::Vector);
    let sel = |h: &Harness<'_, PhotocraftApp>| h.state().session.active().unwrap().doc.selection.clone();
    click_with(&mut h, r.center(), Modifiers::COMMAND);
    let b = sel(&h).expect("⌘-click selects the path").content_bounds();
    assert_eq!((b.x0, b.y0, b.width(), b.height()), (20, 10, 100, 80));
    assert!(!h.state().ui.vector_mask_target, "⌘-click doesn't retarget");
    // ⌘⇧ add a 4×4 square, ⌘⌥ subtract, ⌘⇧⌥ intersect.
    h.state_mut().run("select.rect", json!({"x": 150, "y": 50, "width": 4, "height": 4})).unwrap();
    click_with(&mut h, r.center(), Modifiers::COMMAND | Modifiers::SHIFT);
    let s1 = sel(&h).unwrap();
    assert!(s1.sample_channel(60, 50, 0) > 0.99 && s1.sample_channel(151, 51, 0) > 0.99, "added");
    h.state_mut().run("select.all", json!({})).unwrap();
    click_with(&mut h, r.center(), Modifiers::COMMAND | Modifiers::ALT);
    let s2 = sel(&h).unwrap();
    assert!(s2.sample_channel(60, 50, 0) < 0.01 && s2.sample_channel(150, 50, 0) > 0.99, "subtracted");
    h.state_mut().run("select.rect", json!({"x": 100, "y": 0, "width": 100, "height": 100})).unwrap();
    click_with(&mut h, r.center(), Modifiers::COMMAND | Modifiers::SHIFT | Modifiers::ALT);
    let b = sel(&h).unwrap().content_bounds();
    assert_eq!((b.x0, b.y0, b.width(), b.height()), (100, 10, 20, 80), "intersected");
}

#[test]
fn clicking_the_vector_mask_targets_it_for_the_path_tools() {
    let (s, masked, _) = session();
    let mut h = harness(s, 0, 1.0, 290.0);
    let r = mask_rect(&h, masked, MaskKind::Vector);
    click_with(&mut h, r.center(), Modifiers::NONE);
    assert!(h.state().ui.vector_mask_target && !h.state().ui.mask_target);
    assert_eq!(h.state().ui.selected_path.as_deref(), Some("layer"), "the Paths panel selects its path");
    assert!(near(super::brackets(&h.ctx).unwrap(), r.expand(3.0)), "brackets on the vector mask");
    assert_eq!(crate::canvas::paint_target(h.state()), json!("pixels"));
    // Path Selection moves the vector mask.
    crate::vector_ui::path_selection_finish(h.state_mut(), [30.0, 30.0], [35.0, 32.0]);
    let k = layer(&h, masked).vector_mask.unwrap().path.subpaths[0].knots[0].anchor;
    assert_eq!((k.x, k.y), (25.0, 12.0));
    // The Pen adds a subpath to it.
    h.state_mut().ui.pen = Some(crate::vector_ui::PenPath { knots: vec![[[150.0, 10.0]; 3], [[190.0, 10.0]; 3], [[190.0, 40.0]; 3]], dragging: false });
    crate::vector_ui::pen_commit(h.state_mut(), true);
    assert_eq!(layer(&h, masked).vector_mask.unwrap().path.subpaths.len(), 2);
    assert!(h.state().session.active().unwrap().doc.work_path.is_none(), "not a work path");
    h.run_steps(2);
    // The pixel mask and the layer thumbnail take the brackets back.
    let m = mask_rect(&h, masked, MaskKind::Pixel);
    click_with(&mut h, m.center(), Modifiers::NONE);
    assert!(h.state().ui.mask_target && !h.state().ui.vector_mask_target);
    assert!(near(super::brackets(&h.ctx).unwrap(), m.expand(3.0)));
    click_with(&mut h, r.center(), Modifiers::NONE);
    let p = layer_thumb_center(&h, masked);
    click_with(&mut h, p, Modifiers::NONE);
    assert!(!h.state().ui.mask_target && !h.state().ui.vector_mask_target);
    // Deleting the vector mask drops the target.
    click_with(&mut h, r.center(), Modifiers::NONE);
    assert!(h.state().ui.vector_mask_target);
    h.state_mut().run("layer.vectorMask.delete", json!({"layer": masked})).unwrap();
    h.run_steps(2);
    assert!(!h.state().ui.vector_mask_target);
}

#[test]
fn mask_view_gestures_fail_gracefully_without_a_mask() {
    let (s, masked, shape) = session();
    let mut h = harness(s, 0, 1.0, 290.0);
    // ⌥-click on a vector mask is not a mask view.
    let p = mask_rect(&h, masked, MaskKind::Vector).center();
    click_with(&mut h, p, Modifiers::ALT);
    assert_eq!(mask_view(&h), serde_json::Value::Null);
    // A layer without a layer mask: the command fails, nothing changes.
    assert!(h.state_mut().run("view.layerMask", json!({"layer": shape, "mode": "gray"})).is_err());
    assert_eq!(mask_view(&h), serde_json::Value::Null);
    h.run_steps(2);
    // A stale view (layer gone) draws nothing instead of panicking.
    h.state_mut().session.active_mut().unwrap().channel_view.layer_mask = Some(photocraft_engine::mask_view_cmds::LayerMaskView {
        layer: photocraft_doc::LayerId(987_654),
        mode: photocraft_engine::mask_view_cmds::MaskViewMode::Gray,
    });
    assert!(canvas_px(&h).is_none());
    h.run_steps(2);
    // The mask goes while it is shown: the view ends.
    h.state_mut().run("view.layerMask", json!({"layer": masked, "mode": "gray"})).unwrap();
    h.state_mut().run("layer.layerMask.delete", json!({"layer": masked})).unwrap();
    h.run_steps(2);
    assert_eq!(mask_view(&h), serde_json::Value::Null);
    assert_eq!(crate::canvas::paint_target(h.state()), json!("pixels"));
}
