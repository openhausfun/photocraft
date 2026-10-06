//! #249 through the real canvas and key handling: Space repositions a marquee being drawn, Space
//! and ⌘Space / ⌘⌥Space are temporary Hand / Zoom tools that give the tool back, D / X and the
//! fill keys are commands whose Keyboard Shortcuts overrides replace the defaults.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use photocraft_geom::Rect;
use serde_json::json;

use super::*;
use crate::canvas::ViewXform;

fn harness(tool: Tool) -> Harness<'static, PhotocraftApp> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
    app.sync_views();
    app.ui.extras.rulers = false;
    app.ui.tool = tool;
    let mut h = Harness::builder().with_size(vec2(1000.0, 700.0)).build_ui_state(
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
    let v = &mut h.state_mut().ui.views[0];
    v.zoom = 1.0;
    v.center = [200.0, 150.0];
    v.fit_pending = false;
    h.run_steps(2);
    h
}

fn screen(h: &Harness<'static, PhotocraftApp>, x: f32, y: f32) -> Pos2 {
    let app = h.state();
    let v = &app.ui.views[0];
    let xf = ViewXform { rect: crate::rulers::content_rect(app, app.last_canvas_rect), zoom: v.zoom, center: v.center, flip: app.ui.view.flip_horizontal };
    xf.to_screen(x, y)
}

/// The platform's modifiers for `Cmd` (⌘ on the Mac, Ctrl elsewhere), as egui-winit reports them.
fn platform(m: Modifiers) -> Modifiers {
    let mut out = Modifiers { alt: m.alt, shift: m.shift, ctrl: m.ctrl, ..Default::default() };
    if m.command {
        out.command = true;
        if cfg!(target_os = "macos") {
            out.mac_cmd = true;
        } else {
            out.ctrl = true;
        }
    }
    out
}

fn key(h: &mut Harness<'static, PhotocraftApp>, k: Key, pressed: bool, m: Modifiers) {
    let m = platform(m);
    h.event(Event::ModifiersChanged(m));
    h.event(Event::Key { key: k, physical_key: None, pressed, repeat: false, modifiers: m });
    h.run_steps(1);
}

fn tap(h: &mut Harness<'static, PhotocraftApp>, k: Key, m: Modifiers) {
    key(h, k, true, m);
    key(h, k, false, m);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
}

fn button(h: &mut Harness<'static, PhotocraftApp>, p: Pos2, down: bool, m: Modifiers) {
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: down, modifiers: platform(m) });
    h.run_steps(1);
}

fn move_to(h: &mut Harness<'static, PhotocraftApp>, x: f32, y: f32) {
    let p = screen(h, x, y);
    h.event(Event::PointerMoved(p));
    h.run_steps(2);
}

fn press_at(h: &mut Harness<'static, PhotocraftApp>, x: f32, y: f32) {
    let p = screen(h, x, y);
    h.event(Event::PointerMoved(p));
    h.run_steps(1);
    button(h, p, true, Modifiers::NONE);
    move_to(h, x + 8.0, y + 6.0);
}

fn selection(h: &Harness<'static, PhotocraftApp>) -> Rect {
    h.state().session.active().unwrap().doc.selection.as_ref().map_or(Rect::EMPTY, |s| s.content_bounds())
}

fn click(h: &mut Harness<'static, PhotocraftApp>, x: f32, y: f32, m: Modifiers) {
    let p = screen(h, x, y);
    h.event(Event::PointerMoved(p));
    h.run_steps(1);
    button(h, p, true, m);
    button(h, p, false, m);
    h.run_steps(1);
}

#[test]
fn space_repositions_a_marquee_being_drawn_then_sizing_resumes() {
    let mut h = harness(Tool::RectMarquee);
    press_at(&mut h, 50.0, 50.0);
    move_to(&mut h, 150.0, 110.0);
    key(&mut h, Key::Space, true, Modifiers::NONE);
    move_to(&mut h, 200.0, 150.0);
    move_to(&mut h, 230.0, 170.0);
    let center = h.state().ui.views[0].center;
    // Released with Space still down: the 100 × 60 rectangle, moved by (80, 60).
    release(&mut h, 230.0, 170.0, Modifiers::NONE);
    key(&mut h, Key::Space, false, Modifiers::NONE);
    h.run_steps(2);
    assert_eq!(selection(&h), Rect::new(130, 110, 230, 170), "offset, same size");
    assert_eq!(h.state().ui.views[0].center, center, "Space didn't pan");
    assert_eq!(h.state().ui.tool, Tool::RectMarquee);

    // Release Space mid-drag: sizing continues from the moved corner.
    let mut h = harness(Tool::EllipseMarquee);
    press_at(&mut h, 50.0, 50.0);
    move_to(&mut h, 150.0, 110.0);
    key(&mut h, Key::Space, true, Modifiers::NONE);
    move_to(&mut h, 170.0, 120.0);
    key(&mut h, Key::Space, false, Modifiers::NONE);
    move_to(&mut h, 200.0, 140.0);
    release(&mut h, 200.0, 140.0, Modifiers::NONE);
    h.run_steps(2);
    let r = selection(&h);
    assert!(r.x0.abs_diff(70) <= 1 && r.y0.abs_diff(60) <= 1 && r.x1.abs_diff(200) <= 1 && r.y1.abs_diff(140) <= 1, "{r:?}");
}

#[test]
fn space_moves_a_lasso_being_drawn() {
    let mut h = harness(Tool::Lasso);
    press_at(&mut h, 50.0, 50.0);
    move_to(&mut h, 120.0, 50.0);
    move_to(&mut h, 120.0, 100.0);
    key(&mut h, Key::Space, true, Modifiers::NONE);
    move_to(&mut h, 170.0, 130.0);
    release(&mut h, 170.0, 130.0, Modifiers::NONE);
    key(&mut h, Key::Space, false, Modifiers::NONE);
    h.run_steps(2);
    let r = selection(&h);
    assert!(r.x0.abs_diff(100) <= 1 && r.y0.abs_diff(80) <= 1 && r.x1.abs_diff(170) <= 1 && r.y1.abs_diff(130) <= 1, "{r:?}");
}

#[test]
fn space_is_a_temporary_hand_and_gives_the_tool_back() {
    let mut h = harness(Tool::Brush);
    key(&mut h, Key::Space, true, Modifiers::NONE);
    assert_eq!(held_tool(h.state(), &h.ctx), Some(Temporary::Hand));
    let before = h.state().ui.views[0].center;
    press_at(&mut h, 100.0, 100.0);
    move_to(&mut h, 160.0, 140.0);
    release(&mut h, 160.0, 140.0, Modifiers::NONE);
    key(&mut h, Key::Space, false, Modifiers::NONE);
    h.run_steps(2);
    let st = h.state();
    assert_ne!(st.ui.views[0].center, before, "the view panned");
    assert_eq!(st.ui.tool, Tool::Brush, "the brush is back");
    assert!(!st.session.active().unwrap().history.can_undo(), "nothing was painted");
}

#[test]
fn temporary_zoom_in_and_out_restore_the_tool() {
    let mut h = harness(Tool::RectMarquee);
    let z0 = h.state().ui.views[0].zoom;
    key(&mut h, Key::Space, true, Modifiers::COMMAND);
    assert_eq!(held_tool(h.state(), &h.ctx), Some(Temporary::ZoomIn));
    click(&mut h, 200.0, 150.0, Modifiers::COMMAND);
    key(&mut h, Key::Space, false, Modifiers::COMMAND);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
    let z1 = h.state().ui.views[0].zoom;
    assert!(z1 > z0, "{z0} -> {z1}");
    assert_eq!(h.state().ui.tool, Tool::RectMarquee);
    assert!(h.state().session.active().unwrap().doc.selection.is_none(), "the click didn't reach the marquee");

    key(&mut h, Key::Space, true, Modifiers::COMMAND | Modifiers::ALT);
    assert_eq!(held_tool(h.state(), &h.ctx), Some(Temporary::ZoomOut));
    click(&mut h, 200.0, 150.0, Modifiers::COMMAND | Modifiers::ALT);
    key(&mut h, Key::Space, false, Modifiers::COMMAND | Modifiers::ALT);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
    assert!(h.state().ui.views[0].zoom < z1);
    assert_eq!(h.state().ui.tool, Tool::RectMarquee);
    assert_eq!(held_tool(h.state(), &h.ctx), None);
}

#[test]
fn temporary_zoom_drag_is_a_scrubby_zoom() {
    let mut h = harness(Tool::Brush);
    let z0 = h.state().ui.views[0].zoom;
    key(&mut h, Key::Space, true, Modifiers::COMMAND);
    press_at(&mut h, 200.0, 150.0);
    move_to(&mut h, 300.0, 150.0);
    // Releasing Space mid-drag keeps zooming until the button comes up.
    key(&mut h, Key::Space, false, Modifiers::COMMAND);
    move_to(&mut h, 320.0, 150.0);
    release(&mut h, 320.0, 150.0, Modifiers::COMMAND);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
    assert!(h.state().ui.views[0].zoom > z0 * 1.5);
    assert_eq!(h.state().ui.tool, Tool::Brush);
    assert!(!h.state().session.active().unwrap().history.can_undo(), "nothing was painted");
}

#[test]
fn rebound_temporary_zoom_uses_the_new_key_only() {
    let mut h = harness(Tool::RectMarquee);
    h.state_mut().run("edit.keyboardShortcuts", json!({"set": {"tools.temporary.zoomIn": "Alt+Z"}})).unwrap();
    key(&mut h, Key::Space, true, Modifiers::COMMAND);
    assert_eq!(held_tool(h.state(), &h.ctx), Some(Temporary::Hand), "⌘Space is no longer zoom; Space still pans");
    key(&mut h, Key::Space, false, Modifiers::COMMAND);
    key(&mut h, Key::Z, true, Modifiers::ALT);
    assert_eq!(held_tool(h.state(), &h.ctx), Some(Temporary::ZoomIn));
    key(&mut h, Key::Z, false, Modifiers::ALT);
    assert_eq!(held_tool(h.state(), &h.ctx), None);
    // Removed: no temporary hand at all.
    h.state_mut().run("edit.keyboardShortcuts", json!({"set": {"tools.temporary.hand": ""}})).unwrap();
    key(&mut h, Key::Space, true, Modifiers::NONE);
    assert_eq!(held_tool(h.state(), &h.ctx), None);
    assert!(!reposition_held(h.state(), &h.ctx));
}

fn fg_bg(h: &Harness<'static, PhotocraftApp>) -> ([f32; 4], [f32; 4]) {
    let t = &h.state().session.tools;
    (t.foreground, t.background)
}

#[test]
fn d_and_x_are_rebindable_commands() {
    let mut h = harness(Tool::Brush);
    h.state_mut().run("tools.setColors", json!({"foreground": "#ff0000", "background": "#00ff00"})).unwrap();
    tap(&mut h, Key::X, Modifiers::NONE);
    assert_eq!(fg_bg(&h), ([0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]), "X swaps");
    tap(&mut h, Key::D, Modifiers::NONE);
    assert_eq!(fg_bg(&h), ([0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]), "D resets");
    // Rebound: the old key no longer fires, the new one does.
    h.state_mut().run("edit.keyboardShortcuts", json!({"set": {"tools.swapColors": "Alt+Shift+X", "tools.defaultColors": ""}})).unwrap();
    h.state_mut().run("tools.setColors", json!({"foreground": "#ff0000", "background": "#00ff00"})).unwrap();
    tap(&mut h, Key::X, Modifiers::NONE);
    tap(&mut h, Key::D, Modifiers::NONE);
    assert_eq!(fg_bg(&h), ([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]), "plain X and D do nothing now");
    tap(&mut h, Key::X, Modifiers::ALT | Modifiers::SHIFT);
    assert_eq!(fg_bg(&h), ([0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]));
}

fn rgba(h: &Harness<'static, PhotocraftApp>, x: i32, y: i32) -> [f32; 4] {
    h.state().session.active().unwrap().doc.layers.last().unwrap().surface().unwrap().rgba(x, y)
}

#[test]
fn fill_keys_fill_and_follow_overrides() {
    let mut h = harness(Tool::Brush);
    h.state_mut().run("layer.new.layer", json!({})).unwrap();
    h.state_mut().run("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
    h.state_mut().run("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20})).unwrap();
    // ⌥⌫: foreground into the selection.
    tap(&mut h, Key::Backspace, Modifiers::ALT);
    assert_eq!(rgba(&h, 15, 15), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(rgba(&h, 50, 50)[3], 0.0);
    // ⇧⌘⌫ with no selection: background into the existing pixels only.
    h.state_mut().run("select.deselect", json!({})).unwrap();
    tap(&mut h, Key::Backspace, Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(rgba(&h, 15, 15), [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(rgba(&h, 50, 50)[3], 0.0, "transparency preserved");
    // ⇧⌥Delete (Windows' Delete key) does the same with the foreground.
    tap(&mut h, Key::Delete, Modifiers::ALT | Modifiers::SHIFT);
    assert_eq!(rgba(&h, 15, 15), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(rgba(&h, 50, 50)[3], 0.0);
    // ⌘⌫: background over the whole layer.
    tap(&mut h, Key::Backspace, Modifiers::COMMAND);
    assert_eq!(rgba(&h, 50, 50), [0.0, 0.0, 1.0, 1.0]);
    // Rebind Fill with Foreground to F9: ⌥⌫ no longer fills.
    h.state_mut().run("edit.keyboardShortcuts", json!({"set": {"edit.fillForeground": "F9"}})).unwrap();
    tap(&mut h, Key::Backspace, Modifiers::ALT);
    assert_eq!(rgba(&h, 50, 50), [0.0, 0.0, 1.0, 1.0], "the default no longer fires");
    tap(&mut h, Key::F9, Modifiers::NONE);
    assert_eq!(rgba(&h, 50, 50), [1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn fill_key_on_a_locked_layer_reports_instead_of_crashing() {
    let mut h = harness(Tool::Brush);
    h.state_mut().run("layer.new.layer", json!({})).unwrap();
    h.state_mut().run("layer.lockLayers", json!({"pixels": true})).unwrap();
    tap(&mut h, Key::Backspace, Modifiers::ALT);
    let st = h.state();
    assert!(st.ui.status_error && st.ui.status.contains("locked"), "{}", st.ui.status);
    assert_eq!(rgba(&h, 5, 5)[3], 0.0);
}

#[test]
fn keyboard_shortcuts_dialog_lists_the_keys_under_tools() {
    let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let items = crate::prefs_ui::shortcut_items(&app);
    let find = |id: &str| items.iter().find(|i| i.0 == id).map(|i| (i.2.clone(), i.3.clone()));
    let tools = || vec!["Tools".to_string()];
    assert_eq!(find("tools.swapColors"), Some((tools(), Some("X".into()))));
    assert_eq!(find("tools.defaultColors"), Some((tools(), Some("D".into()))));
    assert_eq!(find("edit.fillForeground"), Some((tools(), Some("Alt+Backspace".into()))));
    assert_eq!(find("edit.fillBackgroundPreserve"), Some((tools(), Some("Cmd+Shift+Backspace".into()))));
    let temp = vec!["Tools".to_string(), "Temporary".to_string()];
    assert_eq!(find("tools.temporary.hand"), Some((temp.clone(), Some("Space".into()))));
    assert_eq!(find("tools.temporary.zoomOut"), Some((temp, Some("Cmd+Alt+Space".into()))));
    assert_eq!(crate::prefs_ui::default_shortcut("tools.temporary.zoomIn").as_deref(), Some("Cmd+Space"));
    // One contiguous Tools section (the dialog prints a header where the top level changes).
    let first = items.iter().position(|i| i.2.first().map(String::as_str) == Some("Tools")).unwrap();
    assert!(items[first..].iter().all(|i| i.2.first().map(String::as_str) == Some("Tools")));
    // Temporary tools never dispatch as pressed commands, even when overridden.
    let mut app = app;
    app.run("edit.keyboardShortcuts", json!({"set": {"tools.temporary.hand": "F10"}})).unwrap();
    assert!(!crate::shortcut_dispatch::bindings(&app).iter().any(|(id, _)| is_temporary(id)));
}

#[test]
fn agents_reposition_a_marquee_with_space() {
    use crate::control::{ControlRequest, Outcome, handle};
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let ctx = egui::Context::default();
    app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
    let mut call = |p: serde_json::Value| {
        let (req, _rx) = ControlRequest::new("ui.pointer", p);
        assert!(matches!(handle(&mut app, &ctx, &req), Outcome::Done(_)));
    };
    call(json!({"tool": "rectMarquee", "events": [{"kind": "down", "x": 20, "y": 20}, {"kind": "move", "x": 120, "y": 80}]}));
    call(json!({"space": true, "events": [{"kind": "move", "x": 170, "y": 110}]}));
    call(json!({"space": true, "events": [{"kind": "up", "x": 170, "y": 110}]}));
    let sel = app.session.active().unwrap().doc.selection.as_ref().unwrap().content_bounds();
    assert_eq!(sel, Rect::new(70, 50, 170, 110));
}

fn release(h: &mut Harness<'static, PhotocraftApp>, x: f32, y: f32, m: Modifiers) {
    let p = screen(h, x, y);
    button(h, p, false, m);
}
