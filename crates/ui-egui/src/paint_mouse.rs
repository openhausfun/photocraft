//! Mouse buttons on the canvas. Tools follow the left button. The right button never paints with
//! the tool: with a painting tool it opens the Brush Preset picker at the pointer (Photoshop), or,
//! with Preferences › Tools › Right-click with painting tools set to Erase, a right drag with the
//! Brush erases with the current brush (Krita, Paint). Each right stroke is one `paint.stroke`
//! with `"erase": true`, so one undo step, with the pen pressure and tilt of a normal stroke.

use egui::{PointerButton, Response};
use photocraft_engine::prefs::RightClickPaint;

use crate::PhotocraftApp;
use crate::state::Tool;

/// Smoothing is a tool option, kept per tool like Photoshop's options bar: switching between the
/// Brush and the Eraser saves the session brush's smoothing for the old tool and loads the new
/// tool's (Photoshop's 10 % the first time). Other tools leave it alone.
pub fn sync_tool_smoothing(app: &mut PhotocraftApp) {
    let tool = app.ui.tool;
    if !matches!(tool, Tool::Brush | Tool::Pencil | Tool::Eraser) || app.ui.smoothing_tool == Some(tool) {
        return;
    }
    let current = app.session.tools.brush.smoothing.clone();
    let next = match app.ui.smoothing_tool {
        // The first smoothing tool takes what the brush has.
        None => current,
        Some(prev) => {
            let saved = &mut app.ui.tool_smoothing;
            match saved.iter_mut().find(|(t, _)| *t == prev) {
                Some(e) => e.1 = current,
                None => saved.push((prev, current)),
            }
            saved
                .iter()
                .find(|(t, _)| *t == tool)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| photocraft_engine::paint::brush::Smoothing { amount: 0.1, ..Default::default() })
        }
    };
    app.session.tools.brush.smoothing = next;
    app.ui.smoothing_tool = Some(tool);
}

/// Which tool events this frame's canvas response produces.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Buttons {
    pub started: bool,
    pub dragged: bool,
    pub stopped: bool,
    pub clicked: bool,
}

/// Tools whose right-click opens the Brush Preset picker (those with the options-bar brush chip).
pub fn has_brush_picker(tool: Tool) -> bool {
    tool.is_brushlike() || tool == Tool::QuickSelection
}

/// Does a right-button drag with `tool` erase (rather than open the picker)?
pub fn right_erases(app: &PhotocraftApp, tool: Tool) -> bool {
    matches!(tool, Tool::Brush | Tool::Eraser) && app.session.prefs().tools.right_click_with_painting_tools == RightClickPaint::Erase
}

/// Route the canvas response's buttons: the left one drives the tool; the right one resizes the
/// brush with Alt held (#297), erases (Erase preference) or opens the Brush Preset picker. Arms
/// `secondary_erase` or `brush_resize_armed` for this frame's `Down`.
pub fn canvas_buttons(app: &mut PhotocraftApp, response: &Response, tool: Tool) -> Buttons {
    let (mods, right_down) = response.ctx.input(|i| (i.modifiers, i.pointer.secondary_down()));
    // Alt+right-drag resizes the brush (brush_resize.rs); its events reach `tool_event` like a
    // left drag's, and nothing paints. A resize whose release was missed ends here.
    crate::brush_resize::release_stale(app, right_down || response.drag_stopped_by(PointerButton::Secondary));
    let resize_start = crate::brush_resize::applies(tool)
        && app.drag.is_none()
        && crate::brush_resize::is_right_gesture(crate::workspace_ui::sticky_mods(app, mods))
        && response.drag_started_by(PointerButton::Secondary);
    let resizing = app.brush_resize.is_some_and(|r| r.secondary);
    app.brush_resize_armed = resize_start;
    // ⌘/Ctrl+right-click lists the layers under the pointer instead (layer_pick_ui.rs, #307).
    let layer_menu = crate::layer_pick_ui::is_gesture(tool, mods);
    let erase = right_erases(app, tool) && !resize_start && !resizing && !layer_menu;
    let right_stroke = erase && app.drag.is_some();
    let right_start = erase && response.drag_started_by(PointerButton::Secondary);
    let right_click = response.secondary_clicked();
    if right_click
        && !erase
        && !layer_menu
        && has_brush_picker(tool)
        && let Some(p) = response.interact_pointer_pos()
    {
        app.ui.brush_picker = Some([p.x, p.y]);
    }
    let erase_click = erase && right_click;
    app.secondary_erase = right_start || erase_click;
    let right_drag = right_stroke || resizing;
    Buttons {
        started: response.drag_started_by(PointerButton::Primary) || right_start || resize_start,
        dragged: response.dragged_by(PointerButton::Primary) || (right_drag && response.dragged_by(PointerButton::Secondary)),
        stopped: response.drag_stopped_by(PointerButton::Primary) || (right_drag && response.drag_stopped_by(PointerButton::Secondary)),
        clicked: response.clicked() || erase_click,
    }
}

/// `ui.pointer` with `"button": "secondary"`: true when its events should reach the tool (an
/// Alt+right-drag brush resize, `brush_resize_armed` for its `Down`; or an erasing right stroke,
/// `secondary_erase` armed for its `Down`). Otherwise a right-click with a painting tool opens
/// the Brush Preset picker over the canvas, and nothing paints.
pub fn pointer_secondary(app: &mut PhotocraftApp, down: bool, mods: egui::Modifiers) -> bool {
    let tool = app.ui.tool;
    if crate::brush_resize::applies(tool) && (crate::brush_resize::is_right_gesture(mods) || app.brush_resize.is_some_and(|r| r.secondary)) {
        app.brush_resize_armed = down && app.drag.is_none();
        return true;
    }
    if right_erases(app, tool) {
        app.secondary_erase = down;
        return true;
    }
    if down && has_brush_picker(tool) {
        let c = app.last_canvas_rect.center();
        app.ui.brush_picker = Some([c.x, c.y]);
    }
    false
}

/// The Brush Preset picker a right-click opened, at the pointer. It edits the session brush like
/// the options-bar chip's; a click outside, Escape or Enter closes it.
pub fn show_picker(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some([x, y]) = app.ui.brush_picker else { return };
    if !has_brush_picker(app.ui.tool) || ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter)) {
        app.ui.brush_picker = None;
        return;
    }
    let screen = ctx.content_rect();
    // Keep the whole picker on screen: its last size, or about 320 × 480 points before it shows.
    let id = egui::Id::new("canvas-brush-picker");
    let size = ctx.memory(|m| m.area_rect(id)).map_or(egui::vec2(324.0, 480.0), |r| r.size());
    let pos = egui::pos2(x.min(screen.right() - size.x).max(screen.left()), y.min(screen.bottom() - size.y).max(screen.top()));
    let area = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        let before = app.session.tools.brush.clone();
        let mut b = before.clone();
        let pick = egui::Frame::popup(ui.style()).show(ui, |ui| crate::brush_picker::body(ui, &mut b, &app.session.tools.presets)).inner;
        crate::brush_panel::commit_gesture(app, ui.ctx(), &before, &b);
        if pick == Some(crate::brush_picker::Pick::OpenSettings) {
            app.ui.brush_picker = None;
        }
        crate::brush_picker::apply(app, ui.ctx(), pick);
    });
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
    if outside {
        app.ui.brush_picker = None;
    }
}

#[cfg(test)]
mod tests {
    use egui::{Modifiers, PointerButton, Pos2, vec2};
    use egui_kittest::Harness;
    use serde_json::json;

    use super::*;
    use crate::canvas::{ToolEvent, tool_event};

    fn harness(prefs: Option<&str>) -> Harness<'static, PhotocraftApp> {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("tools.setColors", json!({"foreground": "#0000ff"})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 20, "hardness": 1.0}})).unwrap();
        if let Some(v) = prefs {
            app.run("prefs.set", json!({"path": "tools.rightClickWithPaintingTools", "value": v})).unwrap();
        }
        app.ui.tool = Tool::Brush;
        app.sync_views();
        let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                let ctx = ui.ctx().clone();
                if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(4);
        h
    }

    fn press(h: &mut Harness<'static, PhotocraftApp>, pos: Pos2, button: PointerButton, pressed: bool) {
        h.event(egui::Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE });
        h.run_steps(2);
    }

    /// Drag across the canvas centre with `button`; returns the document points it covered.
    fn drag(h: &mut Harness<'static, PhotocraftApp>, button: PointerButton) -> (Pos2, Pos2) {
        let c = h.state().last_canvas_rect.center();
        let (a, b) = (c - vec2(80.0, 0.0), c + vec2(80.0, 0.0));
        h.event(egui::Event::PointerMoved(a));
        h.run_steps(1);
        press(h, a, button, true);
        for i in 1..=8 {
            h.event(egui::Event::PointerMoved(a + (b - a) * (i as f32 / 8.0)));
            h.run_steps(1);
        }
        press(h, b, button, false);
        (a, b)
    }

    fn alpha_at(h: &Harness<'static, PhotocraftApp>, screen: Pos2) -> f32 {
        let app = h.state();
        let v = app.ui.views[0].clone();
        let r = app.last_canvas_rect;
        let d = (screen - r.center()) / v.zoom;
        let (x, y) = ((d.x + v.center[0]) as i32, (d.y + v.center[1]) as i32);
        let st = app.session.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)[3]
    }

    fn strokes(h: &Harness<'static, PhotocraftApp>) -> Vec<serde_json::Value> {
        h.state().session.journal.iter().filter(|(id, _)| id == "paint.stroke").map(|(_, p)| p.clone()).collect()
    }

    #[test]
    fn left_drag_paints() {
        let mut h = harness(None);
        let (a, b) = drag(&mut h, PointerButton::Primary);
        assert_eq!(strokes(&h).len(), 1);
        assert!(alpha_at(&h, a + (b - a) * 0.5) > 0.9);
    }

    #[test]
    fn right_click_opens_the_brush_picker_and_never_paints() {
        let mut h = harness(None);
        let undo = h.state().session.active().unwrap().history.past_len();
        let c = h.state().last_canvas_rect.center();
        h.event(egui::Event::PointerMoved(c));
        h.run_steps(1);
        press(&mut h, c, PointerButton::Secondary, true);
        press(&mut h, c, PointerButton::Secondary, false);
        assert_eq!(h.state().ui.brush_picker, Some([c.x, c.y]), "opened at the pointer");
        assert!(strokes(&h).is_empty());
        // The picker shows over the canvas at the pointer, moved up just enough to stay on screen.
        h.run_steps(2);
        let shown = h.ctx.memory(|m| m.area_rect(egui::Id::new("canvas-brush-picker"))).expect("picker shown");
        let screen = h.ctx.content_rect();
        assert!((shown.min.x - c.x).abs() < 1.0 && shown.min.y <= c.y + 1.0, "{shown:?}");
        assert!(screen.contains_rect(shown.shrink(0.5)) && shown.width() > 250.0 && shown.height() > 150.0, "{shown:?}");
        // It is the real preset library, not a fixed set of round tips (#258).
        let names: Vec<String> = h.state().session.tools.presets.iter().take(3).map(|p| p.name.clone()).collect();
        {
            use egui_kittest::kittest::Queryable;
            for n in &names {
                assert!(h.query_by_label(n).is_some(), "{n} listed");
            }
        }
        // A right drag doesn't paint either.
        drag(&mut h, PointerButton::Secondary);
        assert!(strokes(&h).is_empty(), "right-drag never paints with the brush picker preference");
        assert_eq!(h.state().session.active().unwrap().history.past_len(), undo, "no edit");
        // Escape closes it.
        h.state_mut().ui.brush_picker = Some([c.x, c.y]);
        h.run_steps(1);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert_eq!(h.state().ui.brush_picker, None);
    }

    #[test]
    fn click_outside_closes_the_picker() {
        let mut h = harness(None);
        let c = h.state().last_canvas_rect.center();
        h.state_mut().ui.brush_picker = Some([c.x, c.y]);
        h.run_steps(2);
        let far = c - vec2(300.0, 200.0);
        h.event(egui::Event::PointerMoved(far));
        press(&mut h, far, PointerButton::Primary, true);
        press(&mut h, far, PointerButton::Primary, false);
        assert_eq!(h.state().ui.brush_picker, None);
    }

    #[test]
    fn right_drag_erases_with_the_erase_preference() {
        let mut h = harness(Some("erase"));
        let (a, b) = drag(&mut h, PointerButton::Primary);
        let mid = a + (b - a) * 0.5;
        assert!(alpha_at(&h, mid) > 0.9);
        let undo = h.state().session.active().unwrap().history.past_len();
        drag(&mut h, PointerButton::Secondary);
        let s = strokes(&h);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1]["erase"], json!(true), "the right stroke erases");
        assert_eq!(alpha_at(&h, mid), 0.0, "erased to transparency");
        assert_eq!(h.state().session.active().unwrap().history.past_len(), undo + 1, "one undo step");
        assert_eq!(h.state().ui.brush_picker, None, "no picker in erase mode");
        // The next left stroke paints again.
        drag(&mut h, PointerButton::Primary);
        assert_eq!(strokes(&h)[2]["erase"], json!(false));
        assert!(alpha_at(&h, mid) > 0.9);
    }

    #[test]
    fn right_stroke_erases_with_pen_pressure_through_the_control_channel_path() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 120, "height": 80})).unwrap();
        app.run("prefs.set", json!({"path": "tools.rightClickWithPaintingTools", "value": "erase"})).unwrap();
        app.ui.tool = Tool::Brush;
        let m = Modifiers::NONE;
        assert!(pointer_secondary(&mut app, true, egui::Modifiers::NONE));
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 40.0, pressure: 0.4 }, m);
        assert!(!app.secondary_erase, "armed for one stroke only");
        tool_event(&mut app, ToolEvent::Move { x: 100.0, y: 40.0, pressure: 0.8 }, m);
        tool_event(&mut app, ToolEvent::Up { x: 100.0, y: 40.0 }, m);
        let p = app.session.journal.iter().rev().find(|(id, _)| id == "paint.stroke").map(|(_, p)| p.clone()).unwrap();
        assert_eq!(p["erase"], json!(true));
        assert_eq!(p["points"][0], json!([10.0, 40.0, 0.4f32 as f64]), "pressure reaches the erasing stroke");
        // On the Background the erasing stroke paints the background colour (#76).
        let st = app.session.active().unwrap();
        assert_eq!(st.doc.layers[0].surface().unwrap().rgba(50, 40), [1.0, 1.0, 1.0, 1.0]);
        // With the default preference the right button opens the picker and nothing paints.
        app.run("prefs.set", json!({"path": "tools.rightClickWithPaintingTools", "value": "brushPicker"})).unwrap();
        assert!(!pointer_secondary(&mut app, true, egui::Modifiers::NONE));
        assert!(app.ui.brush_picker.is_some() && !app.secondary_erase);
    }

    #[test]
    fn smoothing_is_a_per_tool_option_that_presets_keep() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 64, "height": 64})).unwrap();
        let amount = |app: &PhotocraftApp| app.session.tools.brush.smoothing.amount;
        app.ui.tool = Tool::Brush;
        sync_tool_smoothing(&mut app);
        assert_eq!(amount(&app), 0.1, "Photoshop's default");
        // Picking a preset (whose own smoothing is 0) keeps the tool's smoothing.
        let preset = app.session.tools.presets.iter().find(|p| p.brush.smoothing.amount == 0.0).map(|p| p.name.clone()).unwrap();
        app.run("tools.setBrush", json!({"preset": preset})).unwrap();
        assert_eq!(amount(&app), 0.1);
        // A user value (the options-bar field) survives a preset too, mode included.
        app.session.tools.brush.smoothing.amount = 0.42;
        app.session.tools.brush.smoothing.pulled_string = true;
        app.run("tools.setBrush", json!({"preset": preset})).unwrap();
        assert_eq!((amount(&app), app.session.tools.brush.smoothing.pulled_string), (0.42, true));
        // The Eraser has its own (10 % at first); switching back restores the Brush's.
        app.ui.tool = Tool::Eraser;
        sync_tool_smoothing(&mut app);
        assert_eq!(amount(&app), 0.1);
        app.session.tools.brush.smoothing.amount = 0.0;
        app.ui.tool = Tool::Brush;
        sync_tool_smoothing(&mut app);
        assert_eq!((amount(&app), app.session.tools.brush.smoothing.pulled_string), (0.42, true));
        // Other tools leave it alone; the Eraser kept its 0 %.
        app.ui.tool = Tool::Move;
        sync_tool_smoothing(&mut app);
        assert_eq!(amount(&app), 0.42);
        app.ui.tool = Tool::Eraser;
        // A stroke picks up the tool's smoothing even without a frame in between.
        tool_event(&mut app, ToolEvent::Down { x: 5.0, y: 5.0, pressure: 1.0 }, Modifiers::NONE);
        assert_eq!(amount(&app), 0.0);
        tool_event(&mut app, ToolEvent::Up { x: 30.0, y: 5.0 }, Modifiers::NONE);
    }

    #[test]
    fn eraser_tool_on_the_background_paints_the_background_colour() {
        // #76: the Eraser on the Background (locked transparency) paints the background colour.
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 120, "height": 80})).unwrap();
        app.run("tools.setColors", json!({"background": "#ff0000"})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 12, "hardness": 1.0}})).unwrap();
        app.ui.tool = Tool::Eraser;
        let m = Modifiers::NONE;
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 40.0, pressure: 1.0 }, m);
        tool_event(&mut app, ToolEvent::Move { x: 100.0, y: 40.0, pressure: 1.0 }, m);
        tool_event(&mut app, ToolEvent::Up { x: 100.0, y: 40.0 }, m);
        let st = app.session.active().unwrap();
        assert_eq!(st.doc.layers[0].name, "Background");
        assert_eq!(st.doc.layers[0].surface().unwrap().rgba(50, 40), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(st.history.undo_label(), Some("Eraser"));
        assert!(!app.ui.status_error, "{}", app.ui.status);
    }

    /// Assert the one committed stroke contains a point at each queued screen position.
    fn assert_committed_points(h: &Harness<'static, PhotocraftApp>, queued: &[Pos2]) {
        let s = strokes(h);
        assert_eq!(s.len(), 1);
        let points = s[0]["points"].as_array().unwrap();
        let app = h.state();
        let v = app.ui.views[0].clone();
        let r = app.last_canvas_rect;
        for p in queued {
            let d = (*p - r.center()) / v.zoom;
            let want = [d.x as f64 + v.center[0] as f64, d.y as f64 + v.center[1] as f64];
            assert!(
                points.iter().any(|q| {
                    let q = q.as_array().unwrap();
                    (q[0].as_f64().unwrap() - want[0]).abs() < 1.0 && (q[1].as_f64().unwrap() - want[1]).abs() < 1.0
                }),
                "queued move {want:?} missing from {points:?}"
            );
        }
    }

    #[test]
    fn one_frame_feeds_every_pointer_move_not_just_the_latest() {
        // Fast strokes used to lose the moves between two frames: the canvas read only
        // `interact_pointer_pos()` once per frame, so the stroke was a coarse polyline. Every
        // `PointerMoved` egui-winit delivered in one frame must now reach the stroke.
        let mut h = harness(None);
        let c = h.state().last_canvas_rect.center();
        let a = c - vec2(90.0, 0.0);
        h.event(egui::Event::PointerMoved(a));
        h.run_steps(1);
        press(&mut h, a, PointerButton::Primary, true);
        // Cross egui's drag threshold on its own frame.
        h.event(egui::Event::PointerMoved(a + vec2(20.0, 0.0)));
        h.run_steps(1);
        // Several moves in ONE frame: `Harness::event` would make one frame per event, so push
        // them straight into the next frame's input. All of them must reach the committed stroke.
        let queued: Vec<Pos2> = (1..=6).map(|i| a + vec2(20.0 + i as f32 * 8.0, 0.0)).collect();
        for p in &queued {
            h.input_mut().events.push(egui::Event::PointerMoved(*p));
        }
        h.run_steps(1);
        let end = *queued.last().unwrap();
        press(&mut h, end, PointerButton::Primary, false);
        assert_committed_points(&h, &queued);
    }
}
