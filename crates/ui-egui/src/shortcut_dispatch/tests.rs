//! #127: every shortcut the UI displays (menu items, shell commands, tool tooltips) dispatches its
//! command, or the item is visibly disabled with a reason — through the whole app, with a
//! realistic layered document, from the places focus usually is.

use egui::accesskit::Role;
use egui::{Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_doc::LayerContent;
use serde_json::json;

use super::{Focus, Outcome, bindings, set_dry_run, take_log};
use crate::PhotocraftApp;
use crate::shortcuts::parse;
use crate::state::Tool;

/// Background, a filled pixel layer "paint" (active, with a selection), a type layer and a group
/// holding a layer: what a real layout PSD looks like.
fn realistic() -> photocraft_engine::Session {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 240, "height": 160})).unwrap();
    s.execute("layer.new.layer", json!({"name": "in-group"})).unwrap();
    s.execute("layer.groupLayers", json!({"name": "group"})).unwrap();
    s.execute("type.create", json!({"x": 20, "y": 60, "text": "Hello"})).unwrap();
    let paint = s.execute("layer.new.layer", json!({"name": "paint"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 20, "width": 80, "height": 60})).unwrap();
    s.execute("edit.fill", json!({"color": "#3070c0"})).unwrap();
    s.execute("layer.select", json!({"layer": paint})).unwrap();
    s
}

fn services() -> crate::Services {
    // Saving is available in the app (an exporter is installed); nothing is written here.
    crate::Services { export: Some(Box::new(|_, _, _| Err("tests don't write files".into()))), ..Default::default() }
}

fn harness() -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(realistic(), services())
    });
    h.run_steps(8);
    h
}

/// The platform's modifiers for a shortcut's: `Cmd` is ⌘ on the Mac and Ctrl elsewhere, as
/// egui-winit reports them (`command` plus the physical key).
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

fn press(h: &mut Harness<'_, PhotocraftApp>, sc: &str) {
    let sc = parse(sc).unwrap_or_else(|| panic!("unparsable shortcut {sc}"));
    let m = platform(sc.modifiers);
    h.event(egui::Event::ModifiersChanged(m));
    h.event(egui::Event::Key { key: sc.logical_key, physical_key: None, pressed: true, repeat: false, modifiers: m });
    h.event(egui::Event::Key { key: sc.logical_key, physical_key: None, pressed: false, repeat: false, modifiers: m });
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
}

fn click(h: &mut Harness<'_, PhotocraftApp>, at: Pos2) {
    h.hover_at(at);
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

fn active_name(h: &Harness<'_, PhotocraftApp>) -> String {
    let st = h.state().session.active().unwrap();
    st.active_layer.and_then(|id| st.doc.layer(id)).map(|l| l.name.clone()).unwrap_or_default()
}

/// Where focus is when the shortcut is pressed.
#[derive(Clone, Copy, Debug)]
enum Place {
    /// The pointer over the canvas, nothing focused.
    Canvas,
    /// Just after clicking the "paint" row in the Layers panel.
    LayersRow,
    /// Just after clicking into the Layers panel's Opacity field (a text field now).
    OpacityField,
    /// A non-text widget focused (Tab from nothing focuses the first one, as egui does).
    FocusedWidget,
}

fn put_focus(h: &mut Harness<'_, PhotocraftApp>, place: Place) {
    match place {
        Place::Canvas => {
            let c = h.state().last_canvas_rect.center();
            h.hover_at(c);
            h.run_steps(2);
        }
        Place::LayersRow => {
            h.get_by_role_and_label(Role::Button, "paint").scroll_to_me();
            h.run_steps(4);
            let row = h.get_by_role_and_label(Role::Button, "paint").rect();
            click(h, row.center() + vec2(row.width() / 4.0, 0.0));
            assert_eq!(active_name(h), "paint");
        }
        Place::OpacityField => {
            // The Layers panel's Opacity field: the DragValue in the right dock reading 100.
            let layers = crate::dock::last_rects(&h.ctx).into_iter().find(|(g, _)| *g == crate::dock::Group::Layers).expect("Layers drawn").1;
            let at = h
                .query_all_by_role(Role::SpinButton)
                .map(|n| n.rect())
                .filter(|r| layers.contains(r.center()))
                .min_by(|a, b| a.center().y.total_cmp(&b.center().y))
                .expect("Layers opacity field")
                .center();
            click(h, at);
            assert_eq!(Focus::of(&h.ctx), Focus::Text, "clicking a value field edits it");
        }
        Place::FocusedWidget => {
            h.event(egui::Event::Key { key: Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
            h.run_steps(2);
            assert_eq!(Focus::of(&h.ctx), Focus::Widget, "Tab focused a widget");
        }
    }
    take_log(&h.ctx);
}

/// Is `bound` the same action as the menu item `shown`? (Window › Layers is `window.panel.layers`
/// in the catalogue and `window.toggle.layers` in the shell.)
fn same(shown: &str, bound: &str) -> bool {
    shown == bound || crate::menus::panel_alias(shown) == Some(bound) || crate::menus::panel_alias(bound) == Some(shown)
}

/// Every shortcut shown in the menus (and the shell's unlisted ones), with whether it is live.
fn displayed(app: &PhotocraftApp) -> Vec<(String, String, bool)> {
    let mut v: Vec<(String, String, bool)> = crate::menus::menu_items(app)
        .into_iter()
        .filter(|i| i.id != "---")
        .filter_map(|i| Some((i.id.clone(), i.shortcut?, crate::menus::is_live(&i.id))))
        .collect();
    for (id, _, menu, sc) in crate::menus::UI_COMMANDS {
        if let Some(sc) = sc
            && menu.is_empty()
        {
            v.push((id.to_string(), sc.to_string(), true));
        }
    }
    v
}

#[test]
fn every_displayed_shortcut_is_bound_to_its_item() {
    let h = harness();
    let app = h.state();
    let table = bindings(app);
    let mut broken = Vec::new();
    for (id, sc, live) in displayed(app) {
        let parsed = parse(&sc).unwrap_or_else(|| panic!("{id}: unparsable {sc}"));
        let owner = table.iter().find(|(_, b)| *b == parsed).map(|(o, _)| o.clone());
        match owner {
            Some(o) if same(&id, &o) => {}
            _ if !live => {
                // Not implemented yet: shown greyed (the parity report tracks these).
                let item = crate::menus::menu_items(app).into_iter().find(|i| i.id == id);
                assert!(item.is_none_or(|i| !i.enabled), "{id} isn't live but shows enabled");
            }
            o => broken.push(format!("{id} shows {sc} but it is bound to {o:?}")),
        }
    }
    assert!(broken.is_empty(), "{} displayed shortcuts don't reach their item:\n{}", broken.len(), broken.join("\n"));
}

/// Press every displayed shortcut from each place focus usually is: each dispatches its command,
/// or the menu shows it disabled and the press reports why. (Dry run: File › Exit, Open, Help
/// links… resolve and check enablement without running.)
#[test]
fn every_displayed_shortcut_dispatches_from_where_focus_usually_is() {
    let mut failures = Vec::new();
    for place in [Place::Canvas, Place::LayersRow, Place::FocusedWidget, Place::OpacityField] {
        let mut h = harness();
        set_dry_run(&h.ctx, true);
        put_focus(&mut h, place);
        let focus = Focus::of(&h.ctx);
        let items = crate::menus::menu_items(h.state());
        for (id, sc, live) in displayed(h.state()) {
            if !live {
                continue;
            }
            let parsed = parse(&sc).unwrap();
            press(&mut h, &sc);
            let log = take_log(&h.ctx);
            if !focus.allows(&parsed) {
                // A focused text field keeps typing / editing keys: nothing may fire.
                if !log.is_empty() {
                    failures.push(format!("{place:?}: {sc} ({id}) fired {log:?} into a focused field"));
                }
                continue;
            }
            let enabled = items.iter().find(|i| i.id == id).map_or_else(|| crate::menus::is_enabled(h.state(), &id), |i| i.enabled);
            match log.as_slice() {
                [(bound, Outcome::Ran)] if same(&id, bound) && enabled => {}
                [(bound, Outcome::Disabled(why))] if same(&id, bound) && !enabled && !why.is_empty() => {}
                other => failures.push(format!("{place:?}: {sc} ({id}, menu enabled: {enabled}) -> {other:?}")),
            }
        }
        // Tool letters from the tools panel tooltips ("Brush Tool  (B)").
        if !matches!(focus, Focus::Text) {
            for t in Tool::ALL.into_iter().filter(|t| t.key() != '\0') {
                h.state_mut().ui.tool = if t.key() == 'H' { Tool::Move } else { Tool::Hand };
                press(&mut h, &t.key().to_string());
                if h.state().ui.tool.key() != t.key() {
                    failures.push(format!("{place:?}: tool key {} left {:?}", t.key(), h.state().ui.tool));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} shortcut failures:\n{}", failures.len(), failures.join("\n"));
}

/// The common shortcuts really run (not a dry run) in the layered document after clicking around
/// the panels: the user report was "shortcut keys don't do anything".
#[test]
fn common_shortcuts_work_after_clicking_panels() {
    for place in [Place::Canvas, Place::LayersRow, Place::FocusedWidget, Place::OpacityField] {
        let mut h = harness();
        put_focus(&mut h, place);
        let count = |h: &Harness<'_, PhotocraftApp>| h.state().session.active().unwrap().doc.walk().len();
        let n = count(&h);
        press(&mut h, "Cmd+J");
        assert_eq!(count(&h), n + 1, "{place:?}: ⌘J duplicates the layer");
        // In a focused text field ⌘Z / ⇧⌘Z undo the typing, as in Photoshop.
        let text = matches!(place, Place::OpacityField);
        if !text {
            press(&mut h, "Cmd+Z");
            assert_eq!(count(&h), n, "{place:?}: ⌘Z undoes");
            press(&mut h, "Cmd+Shift+Z");
            assert_eq!(count(&h), n + 1, "{place:?}: ⇧⌘Z redoes");
            press(&mut h, "Cmd+Z");
        }
        press(&mut h, "Cmd+D");
        assert!(h.state().session.active().unwrap().doc.selection.is_none(), "{place:?}: ⌘D deselects");
        press(&mut h, "Cmd+Shift+D");
        assert!(h.state().session.active().unwrap().doc.selection.is_some(), "{place:?}: ⇧⌘D reselects");
        let z = h.state().current_zoom();
        press(&mut h, "Cmd+=");
        assert!(h.state().current_zoom() > z, "{place:?}: ⌘+ zooms in");
        let rulers = h.state().ui.extras.rulers;
        press(&mut h, "Cmd+R");
        assert_ne!(h.state().ui.extras.rulers, rulers, "{place:?}: ⌘R toggles rulers");
        let layers = h.state().ui.panels.layers;
        press(&mut h, "F7");
        assert_ne!(h.state().ui.panels.layers, layers, "{place:?}: F7 toggles Layers");
        press(&mut h, "F7");
        // Dialogs: Image Size (⌥⌘I, shown in the menu but unbound before #127) and Levels (⌘L).
        for (sc, what) in [("Cmd+Alt+I", "Image Size"), ("Cmd+Alt+C", "Canvas Size"), ("Cmd+L", "Levels"), ("Cmd+M", "Curves")] {
            press(&mut h, sc);
            assert_eq!(h.state().ui.dialogs.len(), 1, "{place:?}: {sc} opens {what}");
            h.state_mut().ui.dialogs.clear();
            h.run_steps(2);
        }
        if !text {
            // From another group (B again would cycle to the Pencil).
            h.state_mut().ui.tool = Tool::Move;
            press(&mut h, "B");
            assert_eq!(h.state().ui.tool, Tool::Brush, "{place:?}: B picks the Brush");
            press(&mut h, "V");
            assert_eq!(h.state().ui.tool, Tool::Move, "{place:?}: V picks Move");
            // The colour keys from the tools panel tooltips: X swaps, D resets.
            let fg = h.state().session.tools.foreground;
            press(&mut h, "X");
            assert_eq!(h.state().session.tools.background, fg, "{place:?}: X swaps colours");
            press(&mut h, "D");
            assert_eq!(h.state().session.tools.foreground, [0.0, 0.0, 0.0, 1.0], "{place:?}: D resets colours");
        }
        press(&mut h, "Cmd+G");
        let group = h.state().session.active().unwrap().active_layer.and_then(|id| h.state().session.active().unwrap().doc.layer(id).cloned());
        assert!(group.is_some_and(|l| matches!(l.content, LayerContent::Group(_))), "{place:?}: ⌘G groups");
    }
}

#[test]
fn a_disabled_shortcut_says_why() {
    let mut h = harness();
    put_focus(&mut h, Place::Canvas);
    // Merge Down on the bottom layer: Photoshop greys it; the key press reports the reason.
    let bg = h.state().session.active().unwrap().doc.layers[0].id;
    h.state_mut().session.select_layer(bg).unwrap();
    h.run_steps(2);
    press(&mut h, "Cmd+E");
    let log = take_log(&h.ctx);
    assert!(matches!(log.as_slice(), [(id, Outcome::Disabled(_) | Outcome::Failed(_))] if id == "layer.mergeLayers"), "{log:?}");
    assert!(!h.state().ui.status.is_empty() && h.state().ui.status_error, "the status bar says why");
}
