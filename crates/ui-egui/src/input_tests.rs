//! Input routing through the real canvas, dialogs and shortcuts (#44, #45): keyboard zoom hits the
//! canvas (never egui's UI scale), and an open dialog keeps the canvas pannable and zoomable but
//! is not cancelled by a click outside it.

use egui::{Key, Modifiers, Pos2, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;

fn harness() -> Harness<'static, PhotocraftApp> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
    app.sync_views();
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            // Fonts set up after the first frame only apply from the next one.
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::shortcuts::handle(app, &ctx);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
            crate::dialogs::show(app, &ctx);
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    h
}

fn zoom(h: &Harness<'static, PhotocraftApp>) -> f32 {
    h.state().current_zoom()
}

fn open_dialog(h: &mut Harness<'static, PhotocraftApp>) {
    crate::dialogs::open_command_dialog(h.state_mut(), "image.adjustments.brightnessContrast", "Brightness/Contrast…");
    h.run_steps(3);
    assert_eq!(h.state().ui.dialogs.len(), 1);
}

/// A point on the canvas well away from the centred dialog.
fn free_canvas(h: &Harness<'static, PhotocraftApp>) -> Pos2 {
    let r = h.state().last_canvas_rect;
    pos2(r.left() + 60.0, r.bottom() - 60.0)
}

#[test]
fn ctrl_plus_zooms_the_canvas_not_the_interface() {
    let mut h = harness();
    let z0 = zoom(&h);
    // ⌘+ / Ctrl+'+' as typed with ⇧ on a US layout, and as the numpad / Nordic `+`.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Plus);
    h.run_steps(2);
    let z1 = zoom(&h);
    assert!(z1 > z0, "⌘+ should zoom the canvas in ({z0} -> {z1})");
    h.key_press_modifiers(Modifiers::COMMAND, Key::Plus);
    h.run_steps(2);
    assert!(zoom(&h) > z1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Minus);
    h.run_steps(2);
    assert!((zoom(&h) - z1).abs() < 1e-4);
    // ⌘1 is 100%, ⌘0 fits on screen again.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Num1);
    h.run_steps(2);
    assert_eq!(zoom(&h), 1.0);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Num0);
    h.run_steps(3);
    assert!((zoom(&h) - z0).abs() < 1e-3, "⌘0 fits on screen: {} vs {z0}", zoom(&h));
    assert_eq!(h.ctx.zoom_factor(), 1.0, "the interface must never scale");
}

#[test]
fn keyboard_zoom_works_with_a_dialog_open() {
    let mut h = harness();
    open_dialog(&mut h);
    let z0 = zoom(&h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Equals);
    h.run_steps(2);
    assert!(zoom(&h) > z0);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Plus);
    h.run_steps(2);
    assert_eq!(h.ctx.zoom_factor(), 1.0);
    assert_eq!(h.state().ui.dialogs.len(), 1);
    // Other commands stay blocked while the dialog is open (⌘J would duplicate the layer).
    let layers = h.state().session.active().unwrap().doc.layers.len();
    h.key_press_modifiers(Modifiers::COMMAND, Key::J);
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().doc.layers.len(), layers);
}

#[test]
fn clicking_outside_a_dialog_keeps_it_open_and_the_canvas_pans_and_zooms() {
    let mut h = harness();
    open_dialog(&mut h);
    let p = free_canvas(&h);
    h.hover_at(p);
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert_eq!(h.state().ui.dialogs.len(), 1, "a click outside must not cancel the dialog");

    // Scrolling over the canvas pans it.
    let c0 = h.state().ui.views[0].center;
    h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -40.0), phase: egui::TouchPhase::Move, modifiers: Modifiers::NONE });
    h.run_steps(8);
    let c1 = h.state().ui.views[0].center;
    assert!(c1[1] > c0[1], "scroll pans under a dialog: {c0:?} -> {c1:?}");

    // Space-drag pans too.
    h.event(egui::Event::Key { key: Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for i in 1..=5 {
        h.hover_at(p + vec2(10.0 * i as f32, 0.0));
        h.run_steps(1);
    }
    h.event(egui::Event::PointerButton { pos: p + vec2(50.0, 0.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.event(egui::Event::Key { key: Key::Space, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    let c2 = h.state().ui.views[0].center;
    let z = zoom(&h);
    assert!((c2[0] - (c1[0] - 50.0 / z)).abs() < 0.5, "space-drag pans by 50 pt: {c1:?} -> {c2:?}");
    assert_eq!(h.state().ui.dialogs.len(), 1);

    // Esc still cancels.
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().ui.dialogs.is_empty());
}

/// Type tool (#206): Alt+←/→ at a collapsed caret kerns the pair before it by 20/1000 em (100
/// with ⌘/Ctrl), one history step per press; ⌘/Ctrl+←/→ moves by word; Alt+Shift+→ extends
/// the selection by a word.
fn wheel(h: &Harness<'static, PhotocraftApp>, dy: f32, modifiers: Modifiers) {
    h.event_modifiers(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, dy), phase: egui::TouchPhase::Move, modifiers }, modifiers);
}

/// The document point under screen point `p` (no flip).
fn doc_at(h: &Harness<'static, PhotocraftApp>, p: Pos2) -> [f32; 2] {
    let v = &h.state().ui.views[0];
    let c = h.state().last_canvas_rect.center();
    [v.center[0] + (p.x - c.x) / v.zoom, v.center[1] + (p.y - c.y) / v.zoom]
}

#[test]
fn alt_scroll_zooms_gently_around_the_pointer() {
    let mut h = harness();
    let r = h.state().last_canvas_rect;
    let p = pos2(r.center().x + 120.0, r.center().y - 70.0);
    h.hover_at(p);
    h.run_steps(2);
    let (z0, d0) = (zoom(&h), doc_at(&h, p));
    // One notch with ⌥ held: +5%, the point under the pointer stays put. The modifiers are
    // released right after the event, while egui still smooths the notch over later frames.
    wheel(&h, 1.0, Modifiers::ALT);
    h.run_steps(40);
    let (z1, d1) = (zoom(&h), doc_at(&h, p));
    assert!((z1 / z0 - 1.05).abs() < 1e-3, "one ⌥ notch is 5%: {z0} -> {z1}");
    assert!((d1[0] - d0[0]).abs() < 0.05 && (d1[1] - d0[1]).abs() < 0.05, "centred on the pointer: {d0:?} -> {d1:?}");
    // Three notches back out.
    wheel(&h, -3.0, Modifiers::ALT);
    h.run_steps(40);
    assert!((zoom(&h) / z1 - 1.05f32.powi(-3)).abs() < 1e-3, "{z1} -> {}", zoom(&h));

    // A plain notch still pans, and does not zoom.
    let (z2, c2) = (zoom(&h), h.state().ui.views[0].center);
    wheel(&h, -1.0, Modifiers::NONE);
    h.run_steps(40);
    assert_eq!(zoom(&h), z2, "a plain scroll never zooms");
    assert!(h.state().ui.views[0].center[1] > c2[1], "a plain scroll pans");

    // ⌘/Ctrl + scroll is still the faster gesture zoom.
    wheel(&h, 1.0, Modifiers::COMMAND);
    h.run_steps(40);
    assert!(zoom(&h) / z2 > 1.05, "⌘-scroll zooms in bigger steps: {z2} -> {}", zoom(&h));
}

#[test]
fn alt_scroll_zooms_while_a_temporary_tool_is_held() {
    let mut h = harness();
    let p = h.state().last_canvas_rect.center();
    h.hover_at(p);
    // Space (the temporary Hand, #249) is down: ⌥ + scroll still zooms by notches, and the
    // held key never changes the current tool.
    h.event(egui::Event::Key { key: Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    let z0 = zoom(&h);
    wheel(&h, 2.0, Modifiers::ALT);
    h.run_steps(40);
    assert!((zoom(&h) / z0 - 1.05f32.powi(2)).abs() < 1e-3, "{z0} -> {}", zoom(&h));
    h.event(egui::Event::Key { key: Key::Space, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    assert_eq!(h.state().ui.tool, crate::state::Tool::Brush);
}

#[test]
fn type_tool_alt_arrows_kern_the_pair() {
    use photocraft_doc::text::Kerning;
    let mut h = harness();
    let id = h.state_mut().run("type.create", json!({"x": 20, "y": 80, "text": "AVA To", "size": 40, "font": "Inter"})).unwrap()["layer"].as_u64().unwrap();
    let text = |h: &Harness<'static, PhotocraftApp>| {
        let st = h.state().session.active().unwrap();
        match &st.doc.layer(photocraft_doc::LayerId(id)).unwrap().content {
            photocraft_doc::LayerContent::Text(t) => t.clone(),
            _ => panic!("not text"),
        }
    };
    let kern = |h: &Harness<'static, PhotocraftApp>| {
        let t = text(h);
        let r = t.char_runs();
        (r[0].style.kerning, r[0].style.kern, r.len())
    };
    let steps = |h: &Harness<'static, PhotocraftApp>| h.state().session.active().unwrap().history.entries().len();
    let metric = photocraft_text::shared().lock().unwrap().pair_kerning(&text(&h), 72.0, 0).unwrap().round();
    h.state_mut().ui.text_edit =
        Some(crate::state::TextEdit { layer: id, caret: 1, anchor: 1, session: "kern-test".into(), created: false, dragging: false, preedit: None });
    h.run_steps(2);
    let s0 = steps(&h);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(kern(&h).0, Kerning::Off);
    assert_eq!(kern(&h).1, metric + 20.0);
    h.key_press_modifiers(Modifiers::ALT | Modifiers::COMMAND, Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(kern(&h).1, metric + 120.0);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(kern(&h).1, metric + 100.0);
    assert_eq!(steps(&h), s0 + 3, "one history step per press");
    // The caret didn't move; ⌘/Ctrl+→ moves by word, Alt+Shift+→ selects by word.
    assert_eq!(h.state().ui.text_edit.as_ref().map(|e| (e.caret, e.anchor)), Some((1, 1)));
    h.key_press_modifiers(Modifiers::COMMAND, Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(h.state().ui.text_edit.as_ref().map(|e| (e.caret, e.anchor)), Some((3, 3)));
    h.key_press_modifiers(Modifiers::ALT | Modifiers::SHIFT, Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(h.state().ui.text_edit.as_ref().map(|e| (e.anchor, e.caret)), Some((3, 6)));
    // With a selection Alt+→ moves by word instead of kerning; at the text end there's no pair.
    let before = kern(&h);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(kern(&h), before);
    assert_eq!(steps(&h), s0 + 3);
    // Undo walks back one press at a time.
    assert!(h.state_mut().session.undo());
    h.run_steps(1);
    assert_eq!(kern(&h).1, metric + 120.0);
}

/// The Character panel's kerning field reads and parses Photoshop's values.
#[test]
fn kerning_field_values() {
    use photocraft_doc::text::Kerning;
    assert_eq!(crate::type_tool::kerning_label((Kerning::Metrics, 0.0)), "Metrics");
    assert_eq!(crate::type_tool::kerning_label((Kerning::Optical, 0.0)), "Optical");
    assert_eq!(crate::type_tool::kerning_label((Kerning::Off, 0.0)), "0");
    assert_eq!(crate::type_tool::kerning_label((Kerning::Off, -49.6)), "-50");
    for (s, v) in [("metrics", Some(json!("metrics"))), (" Optical ", Some(json!("optical"))), ("120", Some(json!(120.0))), ("-25.4", Some(json!(-25.0)))] {
        assert_eq!(crate::type_tool::parse_kerning(s), v, "{s}");
    }
    for s in ["", "tight", "1e9", "-5000", "NaN", "inf"] {
        assert_eq!(crate::type_tool::parse_kerning(s), None, "{s}");
    }
}
