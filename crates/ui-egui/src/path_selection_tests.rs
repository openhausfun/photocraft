//! #306: ⌘↩ (Ctrl+Enter off the Mac) loads a path as a selection, as in Photoshop: the path
//! being drawn with the Pen, the path selected in the Paths panel, or the work path. The key is
//! rebindable in Edit › Keyboard Shortcuts, and with no path it says why instead of doing nothing.

use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::vector_ui::PenPath;

fn harness() -> Harness<'static, PhotocraftApp> {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 200, "height": 100})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(s, crate::Services::default())
    });
    h.run_steps(6);
    h
}

/// ⌘ on the Mac, Ctrl elsewhere, as egui-winit reports them.
fn command() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers { command: true, mac_cmd: true, ..Default::default() }
    } else {
        Modifiers { command: true, ctrl: true, ..Default::default() }
    }
}

fn press(h: &mut Harness<'_, PhotocraftApp>, key: Key, m: Modifiers) {
    h.event(Event::ModifiersChanged(m));
    h.event(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m });
    h.event(Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m });
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(3);
}

fn rect_path(x: f64, y: f64, w: f64, h: f64) -> serde_json::Value {
    let k = |x: f64, y: f64| json!({"anchor": [x, y]});
    json!({"subpaths": [{"closed": true, "knots": [k(x, y), k(x + w, y), k(x + w, y + h), k(x, y + h)]}]})
}

/// The selection's bounds (x, y, width, height), if any.
fn selection(h: &Harness<'_, PhotocraftApp>) -> Option<[i64; 4]> {
    let r = h.state().session.active()?.doc.selection.as_ref()?.content_bounds();
    Some([r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0].map(i64::from))
}

#[test]
fn cmd_enter_with_the_pen_loads_the_work_path() {
    let mut h = harness();
    h.state_mut().run("path.set", json!({"name": "work", "path": rect_path(10.0, 10.0, 50.0, 40.0)})).unwrap();
    h.state_mut().ui.tool = Tool::Pen;
    h.run_steps(2);
    assert!(h.state().session.active().unwrap().doc.selection.is_none());
    press(&mut h, Key::Enter, command());
    assert!(h.state().session.active().unwrap().doc.selection.is_some(), "⌘↩ made a selection: {}", h.state().ui.status);
    assert_eq!(selection(&h), Some([10, 10, 50, 40]));
}

#[test]
fn cmd_enter_while_drawing_finishes_the_path_and_loads_it() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Pen;
    let p = |x: f64, y: f64| [[x, y], [x, y], [x, y]];
    h.state_mut().ui.pen = Some(PenPath { knots: vec![p(20.0, 20.0), p(120.0, 20.0), p(120.0, 80.0)], dragging: false });
    h.run_steps(2);
    press(&mut h, Key::Enter, command());
    let app = h.state();
    assert!(app.ui.pen.is_none(), "the pen path was finished");
    assert!(app.session.active().unwrap().doc.work_path.is_some(), "into the work path");
    assert!(app.session.active().unwrap().doc.selection.is_some(), "and loaded as a selection: {}", app.ui.status);
    // Plain ↩ still only finishes a path.
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Pen;
    h.state_mut().ui.pen = Some(PenPath { knots: vec![p(20.0, 20.0), p(120.0, 20.0), p(120.0, 80.0)], dragging: false });
    h.run_steps(2);
    press(&mut h, Key::Enter, Modifiers::NONE);
    assert!(h.state().session.active().unwrap().doc.work_path.is_some());
    assert!(h.state().session.active().unwrap().doc.selection.is_none());
}

#[test]
fn cmd_enter_loads_the_path_selected_in_the_paths_panel() {
    let mut h = harness();
    h.state_mut().run("path.set", json!({"name": "work", "path": rect_path(0.0, 0.0, 30.0, 30.0)})).unwrap();
    h.state_mut().run("path.set", json!({"name": "Path 1", "path": rect_path(100.0, 40.0, 60.0, 50.0)})).unwrap();
    // Any tool, with a path selected in the Paths panel (as in Photoshop).
    h.state_mut().ui.tool = Tool::Brush;
    h.state_mut().ui.selected_path = Some("Path 1".into());
    h.run_steps(2);
    press(&mut h, Key::Enter, command());
    assert_eq!(selection(&h), Some([100, 40, 60, 50]), "{}", h.state().ui.status);
    // A selected path that went away falls back to the work path.
    h.state_mut().run("path.delete", json!({"name": "Path 1"})).unwrap();
    h.state_mut().run("select.deselect", json!({})).unwrap();
    press(&mut h, Key::Enter, command());
    assert_eq!(selection(&h), Some([0, 0, 30, 30]));
}

#[test]
fn without_a_path_it_says_why_and_the_key_can_be_rebound() {
    let mut h = harness();
    h.state_mut().ui.tool = Tool::Pen;
    press(&mut h, Key::Enter, command());
    let app = h.state();
    assert!(app.session.active().unwrap().doc.selection.is_none());
    assert!(app.ui.status_error && app.ui.status.contains("No path"), "status: {}", app.ui.status);
    // It's a listed, rebindable shortcut.
    let cmd_enter = crate::shortcuts::parse("Cmd+Enter").unwrap();
    assert!(crate::shortcut_dispatch::bindings(app).iter().any(|(id, sc)| id == "path.toSelection" && *sc == cmd_enter));
    h.state_mut().run("edit.keyboardShortcuts", json!({"set": {"path.toSelection": "Cmd+Shift+Y"}})).unwrap();
    h.state_mut().run("path.set", json!({"name": "work", "path": rect_path(10.0, 10.0, 20.0, 20.0)})).unwrap();
    press(&mut h, Key::Enter, command());
    assert!(h.state().session.active().unwrap().doc.selection.is_none(), "⌘↩ was moved");
    let mut m = command();
    m.shift = true;
    press(&mut h, Key::Y, m);
    assert_eq!(selection(&h), Some([10, 10, 20, 20]), "{}", h.state().ui.status);
}
