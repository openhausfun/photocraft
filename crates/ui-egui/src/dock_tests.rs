//! Dock layout tests (#88): fixed group heights, splitters, collapse, reorder, persistence.

use egui::{Modifiers, PointerButton, Pos2, Rect, vec2};
use egui_kittest::Harness;
use serde_json::json;

use super::*;
use crate::theme::ThemeKind;

const ESSENTIALS: [Group; 3] = [Group::Color, Group::Properties, Group::Layers];

#[test]
fn last_expanded_group_fills_the_column() {
    let l = DockLayout::default();
    let hs = l.heights_for(&ESSENTIALS, 1200.0, 28.0);
    assert_eq!(hs[0], (Group::Color, Group::Color.default_height()));
    assert_eq!(hs[1], (Group::Properties, Group::Properties.default_height()));
    let total: f32 = hs.iter().map(|(_, h)| h).sum::<f32>() + 2.0 * GAP;
    assert!((total - 1200.0).abs() < 1e-3, "{hs:?}");
    // Collapsed groups shrink to the tab strip; the one above Layers keeps its height.
    let mut l = DockLayout::default();
    l.set_collapsed(Group::Color, true);
    let hs = l.heights_for(&ESSENTIALS, 1000.0, 28.0);
    assert_eq!(hs[0].1, 28.0);
    assert_eq!(hs[1].1, Group::Properties.default_height());
    // Layers collapsed: Properties becomes the filler.
    l.set_collapsed(Group::Layers, true);
    let hs = l.heights_for(&ESSENTIALS, 800.0, 28.0);
    assert_eq!(hs[2].1, 28.0);
    assert!((hs[1].1 - (800.0 - 56.0 - 2.0 * GAP)).abs() < 1e-3);
}

#[test]
fn short_columns_squeeze_groups_down_to_their_minimum_and_never_go_negative() {
    let l = DockLayout::default();
    let hs = l.heights_for(&ESSENTIALS, 400.0, 28.0);
    assert_eq!(hs[2].1, Group::Layers.min_height(), "Layers keeps its minimum: {hs:?}");
    assert_eq!(hs[0].1, Group::Color.compact_height(), "the group farthest from Layers gives way last");
    assert!(hs[1].1 < Group::Properties.compact_height());
    for avail in [0.0, -50.0, 1.0, f32::NAN, f32::INFINITY, 1e9] {
        for (g, h) in l.heights_for(&ESSENTIALS, avail, 28.0) {
            assert!(h.is_finite() && h >= 0.0, "{g:?} at {avail}: {h}");
        }
    }
    assert!(l.heights_for(&[], 500.0, 28.0).is_empty());
}

#[test]
fn bad_stored_values_are_sanitised() {
    let mut l = DockLayout { order: vec![Group::Layers, Group::Layers, Group::Color], ..Default::default() };
    assert_eq!(l.order(), vec![Group::Layers, Group::Color, Group::Properties, Group::Character, Group::Navigator, Group::History]);
    for bad in [f32::NAN, -10.0, f32::INFINITY, 1e12] {
        l.heights.insert(Group::Color, bad);
        let h = l.height(Group::Color);
        assert!(h.is_finite() && h >= Group::Color.min_height() && h <= MAX_HEIGHT, "{bad} -> {h}");
    }
    // Unknown fields, wrong types and old UI state all load.
    let back: DockLayout = serde_json::from_value(json!({"order": ["layers"], "bogus": 1})).unwrap();
    assert_eq!(back.order(), vec![Group::Layers, Group::Color, Group::Properties, Group::Character, Group::Navigator, Group::History]);
    let mut ui = serde_json::to_value(crate::state::UiState::default()).unwrap();
    ui.as_object_mut().unwrap().remove("dock");
    let ui: crate::state::UiState = serde_json::from_value(ui).unwrap();
    assert_eq!(ui.dock, DockLayout::default());
}

#[test]
fn move_group_reorders() {
    let mut l = DockLayout::default();
    l.move_group(Group::Layers, Some(Group::Color));
    assert_eq!(l.order()[0], Group::Layers);
    l.move_group(Group::Layers, None);
    assert_eq!(l.order().last(), Some(&Group::Layers));
    l.move_group(Group::Color, Some(Group::Color));
    assert_eq!(l.order()[0], Group::Color);
}

fn app_with_layers() -> (PhotocraftApp, photocraft_doc::LayerId, photocraft_doc::LayerId) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 200, "height": 150})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    let pixel = app.session.active().unwrap().active_layer.unwrap();
    app.run("layer.newAdjustmentLayer.curves", json!({})).unwrap();
    let adj = app.session.active().unwrap().active_layer.unwrap();
    assert_ne!(pixel, adj);
    app.sync_views();
    (app, pixel, adj)
}

fn harness(app: PhotocraftApp, size: egui::Vec2, theme: ThemeKind) -> Harness<'static, PhotocraftApp> {
    // 60 fps steps, so two clicks a frame apart count as a double-click.
    let mut h = Harness::builder().with_size(size).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::panels::right_dock(app, ui);
            egui::CentralPanel::default().show(ui, |_| {});
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, theme);
    h.state_mut().ui.theme = theme;
    h.run_steps(4);
    h
}

fn rect_of(h: &Harness<'static, PhotocraftApp>, g: Group) -> Rect {
    last_rects(&h.ctx).into_iter().find(|(x, _)| *x == g).map(|(_, r)| r).unwrap_or_else(|| panic!("{g:?} not drawn"))
}

fn drag(h: &mut Harness<'static, PhotocraftApp>, from: Pos2, to: Pos2) {
    h.event(egui::Event::PointerMoved(from));
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for i in 1..=6 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.run_steps(1);
    }
    h.event(egui::Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

#[test]
fn switching_layer_kinds_keeps_the_layers_panel_still() {
    for theme in [ThemeKind::ProMedium, ThemeKind::Studio] {
        let (app, pixel, adj) = app_with_layers();
        let mut h = harness(app, vec2(1200.0, 800.0), theme);
        let rects = |h: &Harness<'static, PhotocraftApp>| last_rects(&h.ctx);
        let with_adj = rects(&h);
        assert!(with_adj.iter().any(|(g, _)| *g == Group::Layers));
        h.state_mut().run("layer.select", json!({"layer": pixel.0})).unwrap();
        h.run_steps(4);
        assert_eq!(rects(&h), with_adj, "{theme:?}: selecting a pixel layer moved the dock groups");
        h.state_mut().run("layer.select", json!({"layer": adj.0})).unwrap();
        h.run_steps(4);
        assert_eq!(rects(&h), with_adj, "{theme:?}: selecting an adjustment layer moved the dock groups");
        // Properties ↔ Adjustments tabs don't move anything either.
        h.state_mut().ui.dock_tabs.properties = 1;
        h.run_steps(3);
        assert_eq!(rects(&h), with_adj);
    }
}

#[test]
fn dragging_the_splitter_resizes_and_survives_a_ui_state_round_trip() {
    let (app, _, _) = app_with_layers();
    let mut h = harness(app, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    let props = rect_of(&h, Group::Properties);
    let layers = rect_of(&h, Group::Layers);
    let split = Pos2::new(props.center().x, props.bottom() + GAP / 2.0);
    let color = rect_of(&h, Group::Color);
    drag(&mut h, split, split - vec2(0.0, 60.0));
    let props2 = rect_of(&h, Group::Properties);
    let layers2 = rect_of(&h, Group::Layers);
    assert!((props2.height() - (props.height() - 60.0)).abs() < 2.0, "{props:?} -> {props2:?}");
    assert!((layers2.top() - (layers.top() - 60.0)).abs() < 2.0, "{layers:?} -> {layers2:?}");
    assert_eq!(rect_of(&h, Group::Color), color, "the group above keeps its place");
    assert_eq!(layers2.bottom(), layers.bottom());
    let stored = h.state().ui.dock.heights.get(&Group::Properties).copied().unwrap();
    // Dragging past the minimum stops at it.
    let split = Pos2::new(props2.center().x, props2.bottom() + GAP / 2.0);
    drag(&mut h, split, split - vec2(0.0, 600.0));
    assert_eq!(rect_of(&h, Group::Properties).height(), Group::Properties.min_height());
    let from = Pos2::new(split.x, rect_of(&h, Group::Properties).bottom() + GAP / 2.0);
    drag(&mut h, from, split);
    assert!((h.state().ui.dock.heights[&Group::Properties] - stored).abs() < 2.0);

    // Save and restore the UI state (what `ui.inspect` / `ui.set` and workspaces carry).
    let saved = serde_json::to_value(&h.state().ui).unwrap();
    let (mut app2, _, _) = app_with_layers();
    app2.ui = serde_json::from_value(saved).unwrap();
    let h2 = harness(app2, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    assert_eq!(last_rects(&h2.ctx), last_rects(&h.ctx));

    // Remembered in the preferences once the mouse is up, and restored at the next launch.
    let prefs = h.state().session.prefs_to_json();
    let mut s2 = photocraft_engine::Session::new();
    s2.load_prefs_json(&prefs).unwrap();
    let mut app3 = PhotocraftApp::new(s2, crate::Services::default());
    restore(&mut app3);
    assert_eq!(app3.ui.dock, h.state().ui.dock);
    // …unless Remember Workspace Changes is off.
    let mut s3 = photocraft_engine::Session::new();
    s3.load_prefs_json(&prefs).unwrap();
    s3.prefs.edit(|p| p.workspace.remember_workspace_changes = false);
    let mut app4 = PhotocraftApp::new(s3, crate::Services::default());
    restore(&mut app4);
    assert_eq!(app4.ui.dock, DockLayout::default());
}

#[test]
fn reset_workspace_restores_the_default_layout_and_new_workspaces_keep_theirs() {
    let (app, _, _) = app_with_layers();
    let mut h = harness(app, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    let ctx = h.ctx.clone();
    let default = last_rects(&h.ctx);
    h.state_mut().ui.dock.heights.insert(Group::Properties, 120.0);
    h.state_mut().ui.dock.set_collapsed(Group::Color, true);
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.newWorkspace", json!({"name": "Tall Layers"})).unwrap();
    let mine = h.state().ui.dock.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.essentials", json!({})).unwrap();
    assert_eq!(h.state().ui.dock, DockLayout::default());
    h.state_mut().ui.dock.heights.insert(Group::Color, 300.0);
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.resetWorkspace", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(last_rects(&h.ctx), default);
    crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.select", json!({"name": "Tall Layers"})).unwrap();
    assert_eq!(h.state().ui.dock, mine);
}

#[test]
fn double_clicking_a_tab_collapses_and_dragging_a_strip_reorders() {
    let (app, _, _) = app_with_layers();
    let mut h = harness(app, vec2(1200.0, 800.0), ThemeKind::ProMedium);
    let color = rect_of(&h, Group::Color);
    let tab = color.left_top() + vec2(20.0, 13.0);
    h.event(egui::Event::PointerMoved(tab));
    h.run_steps(1);
    for _ in 0..2 {
        h.event(egui::Event::PointerButton { pos: tab, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.event(egui::Event::PointerButton { pos: tab, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(2);
    assert!(h.state().ui.dock.is_collapsed(Group::Color));
    assert!(rect_of(&h, Group::Color).height() < 40.0);
    h.state_mut().ui.dock.set_collapsed(Group::Color, false);
    h.run_steps(3);
    // Drag the Layers tab strip (right of its tabs) above Color.
    let layers = rect_of(&h, Group::Layers);
    let strip = Pos2::new(layers.right() - 60.0, layers.top() + 13.0);
    let to = rect_of(&h, Group::Color).left_top() + vec2(120.0, 10.0);
    drag(&mut h, strip, to);
    assert_eq!(h.state().ui.dock.order().first(), Some(&Group::Layers));
    // A locked workspace keeps the order.
    h.state_mut().session.prefs.edit(|p| p.workspace_locked = true);
    let layers = rect_of(&h, Group::Layers);
    drag(&mut h, Pos2::new(layers.right() - 60.0, layers.top() + 13.0), Pos2::new(layers.right() - 60.0, 790.0));
    assert_eq!(h.state().ui.dock.order().first(), Some(&Group::Layers));
}

#[test]
fn tiny_windows_do_not_panic() {
    for theme in [ThemeKind::ProMedium, ThemeKind::Studio] {
        for (w, ht) in [(40.0, 30.0), (300.0, 80.0), (600.0, 200.0), (2000.0, 120.0)] {
            let (mut app, _, _) = app_with_layers();
            app.ui.panels.history = true;
            app.ui.panels.navigator = true;
            let mut h = harness(app, vec2(w, ht), theme);
            for (_, r) in last_rects(&h.ctx) {
                assert!(r.height() >= 0.0 && r.is_finite());
            }
            // Drag a splitter way off-screen.
            if let Some((_, r)) = last_rects(&h.ctx).first().copied() {
                let p = Pos2::new(r.center().x, r.bottom() + GAP / 2.0);
                drag(&mut h, p, p + vec2(0.0, 5000.0));
                drag(&mut h, p, p - vec2(0.0, 5000.0));
            }
        }
    }
}

#[test]
fn the_rail_and_window_menu_never_lose_a_panel() {
    let (mut app, _, _) = app_with_layers();
    let ctx = egui::Context::default();
    // The rail collapses and expands a docked group; it never hides it (#129).
    rail_click(&mut app, Group::Layers, true);
    assert!(app.ui.panels.layers && app.ui.dock.is_collapsed(Group::Layers));
    rail_click(&mut app, Group::Layers, true);
    assert!(app.ui.panels.layers && !app.ui.dock.is_collapsed(Group::Layers));
    // Window › Layers on a collapsed group expands it instead of hiding it.
    app.ui.dock.set_collapsed(Group::Layers, true);
    crate::menus::invoke(&mut app, &ctx, "window.panel.layers", json!({})).unwrap();
    assert!(app.ui.panels.layers && !app.ui.dock.is_collapsed(Group::Layers));
    app.ui.dock.set_collapsed(Group::Color, true);
    crate::menus::invoke(&mut app, &ctx, "window.toggle.color", json!({})).unwrap();
    assert!(app.ui.panels.color && !app.ui.dock.is_collapsed(Group::Color));
    app.ui.dock.set_collapsed(Group::Color, true);
    crate::menus::invoke(&mut app, &ctx, "window.panel.gradients", json!({})).unwrap();
    assert!(app.ui.panels.color && !app.ui.dock.is_collapsed(Group::Color) && app.ui.dock_tabs.color == 2);
    // Hidden panels come back from the Window menu, expanded, on their tab.
    app.ui.panels.history = false;
    app.ui.dock.set_collapsed(Group::History, true);
    crate::menus::invoke(&mut app, &ctx, "window.panel.actions", json!({})).unwrap();
    assert!(app.ui.panels.history && !app.ui.dock.is_collapsed(Group::History) && app.ui.dock_tabs.history == 1);
    // Reset Workspace brings back everything the preset shows.
    app.ui.panels = crate::state::Panels { layers: false, properties: false, color: false, ..Default::default() };
    app.ui.dock_tabs.layers = 2;
    app.ui.dock.set_collapsed(Group::Properties, true);
    crate::menus::invoke(&mut app, &ctx, "window.workspace.resetWorkspace", json!({})).unwrap();
    assert!(app.ui.panels.layers && app.ui.panels.properties && app.ui.panels.color);
    assert_eq!(app.ui.dock, DockLayout::default());
    assert_eq!(app.ui.dock_tabs.layers, 0);
}

/// #129: clicking around the whole UI (layers, canvas, tools, options bar) never switches,
/// hides or moves a dock panel.
#[test]
fn clicking_around_the_ui_keeps_the_panels_put() {
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let (app, _, _) = app_with_layers();
        app
    });
    h.run_steps(8);
    let panels = h.state().ui.panels.clone();
    let tabs = h.state().ui.dock_tabs;
    let rects = last_rects(&h.ctx);
    assert!(rects.iter().any(|(g, _)| *g == Group::Layers), "{rects:?}");
    let layers = rect_of(&h, Group::Layers);
    let canvas = h.state().last_canvas_rect;
    let mut points: Vec<Pos2> = Vec::new();
    // Layer rows (eyes, thumbnails, names) and the Layers footer.
    for dy in [118.0, 140.0, 150.0, 170.0, 182.0, 200.0] {
        for x in [layers.left() + 22.0, layers.left() + 50.0, layers.center().x] {
            points.push(Pos2::new(x, layers.top() + dy));
        }
    }
    // The canvas, the toolbar column and the options bar.
    points.extend([canvas.center(), canvas.left_top() + vec2(40.0, 40.0), canvas.right_bottom() - vec2(30.0, 30.0)]);
    for y in (110..460).step_by(33) {
        points.push(Pos2::new(20.0, y as f32));
    }
    for x in (40..1100).step_by(70) {
        points.push(Pos2::new(x as f32, 50.0));
    }
    for p in points {
        h.hover_at(p);
        h.run_steps(1);
        h.drag_at(p);
        h.run_steps(1);
        h.drop_at(p);
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        // Popups and dialogs opened by a click are fine; panels changing aren't.
        let app = h.state();
        assert_eq!(app.ui.panels.layers, panels.layers, "click at {p:?} hid/showed Layers");
        assert_eq!(app.ui.panels.properties, panels.properties, "click at {p:?} hid/showed Properties");
        assert_eq!(app.ui.panels.color, panels.color, "click at {p:?} hid/showed Color");
        assert_eq!(app.ui.dock_tabs, tabs, "click at {p:?} switched a dock tab");
        assert!(app.ui.dock.collapsed.is_empty(), "click at {p:?} collapsed a group");
        if app.ui.dialogs.is_empty() && app.ui.shell.dialog.is_none() {
            assert_eq!(last_rects(&h.ctx), rects, "click at {p:?} moved the dock groups");
        }
        h.state_mut().ui.dialogs.clear();
    }
}

fn is_pro(theme: ThemeKind) -> bool {
    matches!(theme, ThemeKind::Pro | ThemeKind::ProMedium)
}

/// Full app at `size` (1× scale) on a document with `n` layers named "Row 00", "Row 01", …
fn app_harness_rows(size: egui::Vec2, theme: ThemeKind, n: usize) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(size).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, theme);
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.ui.theme = theme;
        app.run("file.new", json!({"width": 200, "height": 150})).unwrap();
        for i in 0..n {
            app.run("layer.new.layer", json!({"name": format!("Row {i:02}")})).unwrap();
        }
        app.sync_views();
        app
    });
    h.run_steps(8);
    h
}

/// Layer rows fully inside the Layers rows viewport.
fn rows_in_view(h: &Harness<'static, PhotocraftApp>, n: usize) -> usize {
    use egui_kittest::kittest::Queryable;
    let layers = rect_of(h, Group::Layers);
    (0..n)
        .filter(|i| {
            h.query_all_by_label(&format!("Row {i:02}"))
                .map(|q| q.rect())
                .any(|r| r.left() >= layers.left() - 1.0 && r.top() >= layers.top() + 26.0 && r.bottom() <= layers.bottom() - 36.0)
        })
        .count()
}

/// #147: the default workspace gives Layers the column's spare height: at least ten rows on a
/// 900 pt window, and a usable list on a 720 pt one.
#[test]
fn default_layout_shows_ten_layer_rows_at_900pt() {
    for (size, want) in [(vec2(1440.0, 900.0), 10), (vec2(1280.0, 720.0), 4)] {
        let h = app_harness_rows(size, ThemeKind::ProMedium, 40);
        let rows = rows_in_view(&h, 40);
        let groups = last_rects(&h.ctx);
        assert!(rows >= want, "{size:?}: {rows} rows visible, want ≥ {want}: {groups:?}");
        // Color and Properties stay open, just not at the expense of Layers.
        for g in [Group::Color, Group::Properties] {
            assert!(rect_of(&h, g).height() >= g.compact_height() - 0.5, "{g:?} at {size:?}: {groups:?}");
        }
    }
}

#[test]
fn defaults_give_way_to_layers_but_user_sizes_stay() {
    let l = DockLayout::default();
    // A tall column: everyone at their defaults, Layers takes the rest.
    let hs = l.heights_for(&ESSENTIALS, 1200.0, 28.0);
    assert_eq!(hs[0].1, Group::Color.default_height());
    assert_eq!(hs[1].1, Group::Properties.default_height());
    // A short one: default-sized groups shrink toward their compact heights so Layers keeps
    // its preferred height, the one nearest Layers first.
    let hs = l.heights_for(&ESSENTIALS, 820.0, 28.0);
    assert!(hs[2].1 >= Group::Layers.preferred_fill() - 0.5, "{hs:?}");
    assert!(hs[1].1 < Group::Properties.default_height() && hs[1].1 >= Group::Properties.compact_height(), "{hs:?}");
    // Heights the user dragged to (saved in prefs) are kept as they are.
    let mut mine = DockLayout::default();
    mine.heights.insert(Group::Properties, 340.0);
    mine.heights.insert(Group::Color, 200.0);
    let hs = mine.heights_for(&ESSENTIALS, 800.0, 28.0);
    assert_eq!((hs[0].1, hs[1].1), (200.0, 340.0), "{hs:?}");
}

/// #150: Window › Character opens a Character | Paragraph group next to Properties (which
/// stays), and toggles closed again; old saved layouts place it above Layers.
#[test]
fn window_character_opens_its_own_group_and_keeps_properties() {
    for theme in [ThemeKind::ProMedium, ThemeKind::Studio] {
        let (app, _, _) = app_with_layers();
        let mut h = harness(app, vec2(1440.0, 900.0), theme);
        let ctx = h.ctx.clone();
        crate::menus::invoke(h.state_mut(), &ctx, "window.panel.character", json!({})).unwrap();
        h.run_steps(3);
        let app = h.state();
        assert!(app.ui.panels.character && app.ui.panels.properties && app.ui.dock_tabs.character == 0, "{theme:?}");
        let ch = rect_of(&h, Group::Character);
        assert!(ch.height() > 60.0, "{theme:?}: Character expanded: {ch:?}");
        if is_pro(theme) {
            let props = rect_of(&h, Group::Properties);
            assert!(props.height() > 60.0 && props.bottom() <= ch.top(), "{theme:?}: Properties visible above Character");
        }
        assert!(rect_of(&h, Group::Layers).top() > ch.bottom(), "{theme:?}: Layers stays the filler at the bottom");
        assert_eq!(crate::view_cmds::checked(h.state(), "window.panel.character"), Some(true));
        // Paragraph is the group's second tab; choosing it again on its tab closes the group.
        crate::menus::invoke(h.state_mut(), &ctx, "window.panel.paragraph", json!({})).unwrap();
        assert!(h.state().ui.panels.character && h.state().ui.dock_tabs.character == 1);
        crate::menus::invoke(h.state_mut(), &ctx, "window.panel.paragraph", json!({})).unwrap();
        h.run_steps(3);
        assert!(!h.state().ui.panels.character && h.state().ui.panels.properties, "{theme:?}");
        assert!(!last_rects(&h.ctx).iter().any(|(g, _)| *g == Group::Character));
        // Type › Panels › Character Panel always shows it; Reset Workspace closes it.
        crate::menus::invoke(h.state_mut(), &ctx, "type.panels.character", json!({})).unwrap();
        crate::menus::invoke(h.state_mut(), &ctx, "type.panels.character", json!({})).unwrap();
        assert!(h.state().ui.panels.character);
        crate::menus::invoke(h.state_mut(), &ctx, "window.workspace.resetWorkspace", json!({})).unwrap();
        assert!(!h.state().ui.panels.character && h.state().ui.panels.properties);
    }
    // A layout saved before the group existed keeps Layers last.
    let old = DockLayout { order: vec![Group::Color, Group::Properties, Group::Navigator, Group::History, Group::Layers], ..Default::default() };
    assert_eq!(old.order(), Group::ALL.to_vec());
    let moved = DockLayout { order: vec![Group::Layers, Group::Color], ..Default::default() };
    assert_eq!(moved.order(), vec![Group::Layers, Group::Color, Group::Properties, Group::Character, Group::Navigator, Group::History]);
}

type StripProbe = Option<(Vec<(usize, Rect)>, Rect, Option<Rect>, Rect)>;

/// #151: at narrow widths and 2× scale, in every theme, tabs elide or overflow into a chevron
/// and never run under the panel menu button.
#[test]
fn tab_strips_never_overlap_the_menu_button() {
    let overlap = |a: Rect, b: Rect| {
        let i = a.intersect(b);
        i.width() > 0.5 && i.height() > 0.5
    };
    for theme in ThemeKind::ALL {
        for scale in [1.0, 2.0] {
            for width in [180.0, 250.0, 290.0, 420.0] {
                for g in Group::ALL {
                    let tabs = g.tabs(is_pro(theme));
                    for sel in 0..tabs.len() {
                        let mut h = Harness::builder().with_size(vec2(width, 200.0)).with_pixels_per_point(scale).build_ui_state(
                            move |ui, out: &mut StripProbe| {
                                if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                                    return;
                                }
                                let mut s = sel;
                                let r = crate::widgets::card_ex(ui, "t", tabs, &mut s, false, |ui, _| {
                                    ui.label("body");
                                });
                                *out = Some((r.tabs, r.menu.rect, r.chevron, ui.max_rect()));
                            },
                            None,
                        );
                        PhotocraftApp::setup_context(&h.ctx, theme);
                        h.run_steps(3);
                        let (shown, menu, chevron, card) = h.state().clone().unwrap();
                        let what = format!("{theme:?} {scale}x {width}pt {g:?} selected {sel}");
                        assert!(shown.iter().any(|(i, _)| *i == sel), "{what}: the selected tab is on the strip: {shown:?}");
                        for (i, r) in &shown {
                            assert!(!overlap(*r, menu), "{what}: tab {i} {r:?} runs under the menu {menu:?}");
                            assert!(r.left() >= card.left() - 0.5 && r.right() <= card.right() + 0.5, "{what}: tab {i} outside the card");
                        }
                        for w in shown.windows(2) {
                            assert!(!overlap(w[0].1, w[1].1), "{what}: tabs overlap");
                        }
                        if let Some(c) = chevron {
                            assert!(!overlap(c, menu) && shown.iter().all(|(_, r)| !overlap(*r, c)), "{what}: chevron overlaps");
                        } else {
                            assert_eq!(shown.len(), tabs.len(), "{what}: a tab went missing without a chevron");
                        }
                    }
                }
            }
        }
    }
}

/// #151 in the real dock: every strip at 2× scale fits beside its menu button, and a tab in the
/// chevron menu can still be chosen.
#[test]
fn dock_strips_fit_and_the_chevron_menu_switches_tabs() {
    for theme in ThemeKind::ALL {
        let (mut app, _, _) = app_with_layers();
        app.ui.panels.history = true;
        app.ui.panels.navigator = true;
        app.ui.panels.character = true;
        let mut h = Harness::builder().with_size(vec2(900.0, 1000.0)).with_pixels_per_point(2.0).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                crate::panels::right_dock(app, ui);
                egui::CentralPanel::default().show(ui, |_| {});
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, theme);
        h.state_mut().ui.theme = theme;
        h.run_steps(4);
        let strips = last_strips(&h.ctx);
        assert!(strips.len() >= 5, "{theme:?}: {strips:?}");
        for s in &strips {
            for (i, r) in &s.tabs {
                assert!(r.intersect(s.menu).width() <= 0.5, "{theme:?} {:?}: tab {i} under the menu", s.group);
            }
        }
    }
    // A strip too narrow for its tabs: pick a hidden tab from the chevron menu.
    let mut h = Harness::builder().with_size(vec2(150.0, 200.0)).build_ui_state(
        |ui, sel: &mut usize| {
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            let _ = crate::widgets::card_ex(ui, "color", &["Color", "Swatches", "Gradients", "Patterns"], sel, false, |ui, _| {
                ui.label("body");
            });
        },
        0usize,
    );
    PhotocraftApp::setup_context(&h.ctx, ThemeKind::ProMedium);
    h.run_steps(3);
    use egui_kittest::kittest::Queryable;
    h.get_by_label("More panels").click();
    h.run_steps(3);
    h.get_by_label("Patterns").click();
    h.run_steps(3);
    assert_eq!(*h.state(), 3, "Patterns chosen from the chevron menu");
}
