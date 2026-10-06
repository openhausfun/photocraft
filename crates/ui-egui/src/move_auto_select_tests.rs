//! Move tool Auto-Select (#296), as in Photoshop: on by default, a press picks the topmost
//! visible layer with pixels under the pointer (or its outermost group in Group mode) and the drag
//! then moves it; ⌘/Ctrl held at the press inverts the option for that click.

use photocraft_doc::LayerId;
use photocraft_geom::Rect;
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::ToolEvent;
use crate::state::{Tool, ToolOptions};

/// A 64×64 document with two painted layers: A covers (8..24)², B covers (40..56)². B is active.
fn app() -> (PhotocraftApp, LayerId, LayerId) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
    app.sync_views();
    let mut ids = Vec::new();
    for at in [8, 40] {
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.session
            .edit("paint", |doc, a| {
                doc.layer_mut(a.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(at, at, at + 16, at + 16), &[1.0, 0.0, 0.0, 1.0]);
                Ok(())
            })
            .unwrap();
        ids.push(app.session.active().unwrap().active_layer.unwrap());
    }
    app.ui.tool = Tool::Move;
    // Snapping (to the document centre with a 1:1 test view) would shift the expected offsets.
    app.ui.extras.snap = false;
    app.ui.view.show.smart_guides = false;
    (app, ids[0], ids[1])
}

fn bounds(app: &PhotocraftApp, id: LayerId) -> Rect {
    app.session.active().unwrap().doc.layer(id).unwrap().surface().unwrap().content_bounds()
}

fn active(app: &PhotocraftApp) -> Option<LayerId> {
    app.session.active().unwrap().active_layer
}

fn drag(app: &mut PhotocraftApp, from: [f64; 2], by: [f64; 2], mods: egui::Modifiers) {
    crate::canvas::tool_event(app, ToolEvent::Down { x: from[0], y: from[1], pressure: 1.0 }, mods);
    let mid = [from[0] + by[0] / 2.0, from[1] + by[1] / 2.0];
    crate::canvas::tool_event(app, ToolEvent::Move { x: mid[0], y: mid[1], pressure: 1.0 }, mods);
    let end = [from[0] + by[0], from[1] + by[1]];
    crate::canvas::tool_event(app, ToolEvent::Move { x: end[0], y: end[1], pressure: 1.0 }, mods);
    crate::canvas::tool_event(app, ToolEvent::Up { x: end[0], y: end[1] }, mods);
}

#[test]
fn auto_select_is_on_by_default_with_the_layer_target() {
    let o = ToolOptions::default();
    assert!(o.move_auto_select, "Photoshop ships with Auto-Select checked");
    assert_eq!(o.move_target, "layer");
    // A saved tool preset that predates the option gets the default too.
    let mut v = serde_json::to_value(&o).unwrap();
    v.as_object_mut().unwrap().remove("move_auto_select");
    let back: ToolOptions = serde_json::from_value(v).unwrap();
    assert!(back.move_auto_select);
}

#[test]
fn a_press_picks_the_layer_under_the_pointer_and_drags_it() {
    let (mut app, a, b) = app();
    assert_eq!(active(&app), Some(b));
    drag(&mut app, [16.0, 16.0], [10.0, 4.0], egui::Modifiers::NONE);
    assert_eq!(active(&app), Some(a), "the click selected A");
    assert_eq!(bounds(&app, a), Rect::new(18, 12, 34, 28), "and the drag moved A");
    assert_eq!(bounds(&app, b), Rect::new(40, 40, 56, 56), "B stays put");
}

#[test]
fn command_click_suspends_auto_select_for_that_drag() {
    let (mut app, a, b) = app();
    drag(&mut app, [16.0, 16.0], [-10.0, 0.0], egui::Modifiers::COMMAND);
    assert_eq!(active(&app), Some(b), "⌘ held: no pick");
    assert_eq!(bounds(&app, b), Rect::new(30, 40, 46, 56), "the selected layer moved");
    assert_eq!(bounds(&app, a), Rect::new(8, 8, 24, 24));
}

#[test]
fn command_click_picks_when_auto_select_is_off() {
    let (mut app, a, b) = app();
    app.ui.tool_options.move_auto_select = false;
    drag(&mut app, [16.0, 16.0], [0.0, 6.0], egui::Modifiers::NONE);
    assert_eq!(active(&app), Some(b), "off: the selected layer moves");
    assert_eq!(bounds(&app, a), Rect::new(8, 8, 24, 24));
    drag(&mut app, [16.0, 16.0], [0.0, 6.0], egui::Modifiers::COMMAND);
    assert_eq!(active(&app), Some(a), "⌘-click picks while the option is off");
    assert_eq!(bounds(&app, a), Rect::new(8, 14, 24, 30));
}

#[test]
fn group_mode_picks_and_moves_the_outermost_group() {
    let (mut app, a, b) = app();
    app.session.execute("layer.select", json!({"layer": a.0})).unwrap();
    let group = LayerId(app.session.execute("layer.new.groupFromLayers", json!({"name": "G"})).unwrap()["layer"].as_u64().unwrap());
    app.session.execute("layer.select", json!({"layer": b.0})).unwrap();
    app.ui.tool_options.move_target = "group".into();
    drag(&mut app, [16.0, 16.0], [0.0, 10.0], egui::Modifiers::NONE);
    assert_eq!(active(&app), Some(group));
    assert_eq!(bounds(&app, a), Rect::new(8, 18, 24, 34), "moving the group moves its layer");
    assert_eq!(bounds(&app, b), Rect::new(40, 40, 56, 56));
}

#[test]
fn dragging_one_of_several_selected_layers_moves_them_all() {
    let (mut app, a, b) = app();
    app.session.execute("layer.select", json!({"layer": a.0})).unwrap();
    app.session.execute("layer.select", json!({"layer": b.0, "mode": "add"})).unwrap();
    drag(&mut app, [16.0, 16.0], [-4.0, -4.0], egui::Modifiers::NONE);
    assert_eq!(bounds(&app, a), Rect::new(4, 4, 20, 20));
    assert_eq!(bounds(&app, b), Rect::new(36, 36, 52, 52));
}

#[test]
fn shift_click_adds_the_layer_under_the_pointer() {
    let (mut app, a, b) = app();
    // A click without a drag only selects.
    drag(&mut app, [16.0, 16.0], [0.0, 0.0], egui::Modifiers::SHIFT);
    let st = app.session.active().unwrap();
    assert!(st.is_layer_selected(a) && st.is_layer_selected(b));
    assert_eq!(bounds(&app, a), Rect::new(8, 8, 24, 24));
}
