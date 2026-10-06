//! Snapping in the shell: builds [`photocraft_engine::snap`] targets from View › Snap To and the
//! document, snaps tool gestures (Move tool, marquees, crop, shapes, pen anchors, Free Transform
//! handles and guides), and draws smart guides (magenta alignment lines) while layers move.
//!
//! Snapping is on when View › Snap (⇧⌘;) is; holding Ctrl while dragging turns it off for that
//! drag, as in Photoshop. Smart guides follow View › Show › Smart Guides and also snap the Move
//! tool to other layers' edges and centres. The threshold is 8 screen pixels at the current zoom.

use egui::{Color32, Stroke, pos2};
use photocraft_doc::LayerId;
use photocraft_engine::snap::{SnapKind, SnapLine, SnapOptions, SnapTargets, layer_rect, union};

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, ViewXform};
use crate::state::Tool;

/// Snap distance in screen pixels.
pub const SNAP_PX: f64 = 8.0;

/// What the current drag snaps.
#[derive(Clone, Debug, PartialEq)]
pub enum Gesture {
    /// A point (marquee corner, crop or shape corner, pen anchor, transform handle).
    Point,
    /// Layers being moved: their bounds at the start of the drag.
    Move { rect: [f64; 4] },
    /// Free Transform box dragged from inside: its bounds at the start of the drag.
    TransformMove { rect: [f64; 4] },
    /// A guide being moved with the Move tool.
    Guide,
}

/// Snapping state of the drag in progress.
#[derive(Clone, Debug)]
pub struct ActiveSnap {
    pub gesture: Gesture,
    pub start: [f64; 2],
    pub targets: SnapTargets,
    /// Layer alignments only (smart guides), used when View › Snap is off.
    pub smart: SnapTargets,
    pub disabled: bool,
}

/// Snap To options from the View menu and the Guides/Grid preferences.
pub fn options(app: &PhotocraftApp) -> SnapOptions {
    let st = &app.ui.view.snap_to;
    let e = &app.ui.extras;
    let grid_step = app.session.active().map_or(18.0, |d| {
        let g = &app.session.prefs().guides_grid_and_slices;
        let ppi = app.session.prefs().units_and_rulers.point_size.per_inch();
        g.major_px(d.doc.resolution_dpi as f64, d.doc.size.width as f64, ppi) / g.subdivisions.max(1) as f64
    });
    SnapOptions { guides: st.guides && e.guides, grid: st.grid && e.grid, layers: st.layers, document: st.document_bounds, selection: true, grid_step }
}

/// Snap tolerance in document pixels at the current zoom.
pub fn tolerance(app: &PhotocraftApp) -> f64 {
    SNAP_PX / app.current_zoom().max(0.01) as f64
}

fn smart_on(app: &PhotocraftApp) -> bool {
    app.ui.view.shows(app.ui.view.show.smart_guides)
}

fn snap_on(app: &PhotocraftApp) -> bool {
    app.ui.extras.snap
}

/// Targets of the active document (smart = layer alignments only).
fn build(app: &PhotocraftApp, exclude: &[LayerId], smart: bool) -> SnapTargets {
    let Some(st) = app.session.active() else { return SnapTargets::default() };
    let opts = if smart { SnapOptions { guides: false, grid: false, layers: true, document: true, selection: false, grid_step: 0.0 } } else { options(app) };
    let t = SnapTargets::from_document(&st.doc, &opts, exclude);
    if smart { t.filtered(SnapKind::is_smart) } else { t }
}

/// Union of the selected layers' bounds (what the Move tool drags).
fn moving_rect(app: &PhotocraftApp) -> Option<[f64; 4]> {
    let st = app.session.active()?;
    union(st.selected_layers().into_iter().filter_map(|id| layer_rect(&st.doc, id)))
}

fn is_point_tool(t: Tool) -> bool {
    matches!(
        t,
        Tool::RectMarquee
            | Tool::EllipseMarquee
            | Tool::Crop
            | Tool::ObjectSelection
            | Tool::Pen
            | Tool::Rectangle
            | Tool::EllipseShape
            | Tool::Triangle
            | Tool::Polygon
            | Tool::Line
            | Tool::Type
    )
}

fn quad_rect(q: &[[f64; 2]; 4]) -> [f64; 4] {
    let xs = q.iter().map(|p| p[0]);
    let ys = q.iter().map(|p| p[1]);
    [xs.clone().fold(f64::MAX, f64::min), ys.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max)]
}

/// Is `p` inside the quad (even-odd)?
fn in_quad(q: &[[f64; 2]; 4], p: [f64; 2]) -> bool {
    let mut inside = false;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 3) % 4]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
    }
    inside
}

/// Near a Free Transform corner or edge handle (within 8 screen px)?
fn near_handle(q: &[[f64; 2]; 4], p: [f64; 2], tol: f64) -> bool {
    (0..4).any(|i| {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        [a, mid].iter().any(|h| (h[0] - p[0]).hypot(h[1] - p[1]) <= tol)
    })
}

/// Photoshop: holding Ctrl (Windows) / Control (macOS) *during* a drag temporarily turns snapping
/// off. Only the held state while dragging counts: Ctrl at the press belongs to the tool (Ctrl-click
/// auto-selects with the Move tool), and ⌘ on the Mac never overrides snapping.
fn override_held(mods: egui::Modifiers) -> bool {
    mods.ctrl && !mods.mac_cmd
}

/// Start snapping for a drag beginning at `p` (called on pointer down).
fn begin(app: &mut PhotocraftApp, p: [f64; 2]) {
    app.prefs_rt.snap = None;
    app.prefs_rt.snap_lines.clear();
    if app.session.active().is_none() {
        return;
    }
    let tool = app.ui.tool;
    let tol = tolerance(app);
    let gesture = if let Some(t) = &app.ui.transform {
        if near_handle(&t.quad, p, tol) {
            Some((Gesture::Point, vec![LayerId(t.layer)]))
        } else if in_quad(&t.quad, p) {
            Some((Gesture::TransformMove { rect: quad_rect(&t.quad) }, vec![LayerId(t.layer)]))
        } else {
            None
        }
    } else if tool == Tool::Move && crate::rulers::guide_at(app, p[0], p[1]).is_some() {
        Some((Gesture::Guide, Vec::new()))
    } else if tool == Tool::Move {
        let exclude = app.session.active().map(|s| s.selected_layers()).unwrap_or_default();
        moving_rect(app).map(|rect| (Gesture::Move { rect }, exclude))
    } else if tool == Tool::Crop
        && let Some(rect) = app.ui.crop_rect.filter(|r| crate::crop_ui::hit(*r, p, tol) == crate::crop_ui::Hit::Inside)
    {
        // Moving the crop frame snaps its edges, like the Move tool's layer bounds.
        Some((Gesture::Move { rect }, Vec::new()))
    } else if is_point_tool(tool) {
        Some((Gesture::Point, Vec::new()))
    } else {
        None
    };
    let Some((gesture, exclude)) = gesture else { return };
    let mut targets = if snap_on(app) { build(app, &exclude, false) } else { SnapTargets::default() };
    if gesture == Gesture::Guide {
        // A guide never snaps to itself (or other guides).
        targets = targets.filtered(|k| k != SnapKind::Guide);
    }
    let smart = if matches!(gesture, Gesture::Move { .. }) && smart_on(app) { build(app, &exclude, true) } else { SnapTargets::default() };
    app.prefs_rt.snap = Some(ActiveSnap { gesture, start: p, targets, smart, disabled: false });
}

/// Round to whole pixels when Preferences › Tools asks vector tools and transforms to snap to
/// the pixel grid.
fn pixel_round(app: &PhotocraftApp, p: [f64; 2]) -> [f64; 2] {
    if app.session.prefs().tools.snap_vector_tools_and_transforms_to_pixel_grid
        && matches!(app.ui.tool, Tool::Pen | Tool::Rectangle | Tool::EllipseShape | Tool::Triangle | Tool::Polygon | Tool::Line)
    {
        [p[0].round(), p[1].round()]
    } else {
        p
    }
}

/// Snap one pointer position for the active gesture; records the alignment lines.
fn apply(app: &mut PhotocraftApp, p: [f64; 2]) -> [f64; 2] {
    let tol = tolerance(app);
    let Some(snap) = app.prefs_rt.snap.as_ref() else { return p };
    if snap.disabled {
        app.prefs_rt.snap_lines.clear();
        return p;
    }
    let (out, lines) = match &snap.gesture {
        Gesture::Point | Gesture::Guide => snap.targets.snap_point(p, tol),
        Gesture::Move { rect } | Gesture::TransformMove { rect } => {
            let d = [p[0] - snap.start[0], p[1] - snap.start[1]];
            let moved = [rect[0] + d[0], rect[1] + d[1], rect[2] + d[0], rect[3] + d[1]];
            let set = if snap.targets.is_empty() { &snap.smart } else { &snap.targets };
            let (adj, lines) = set.snap_rect(moved, tol);
            ([p[0] + adj[0], p[1] + adj[1]], lines)
        }
    };
    app.prefs_rt.snap_lines = lines;
    out
}

/// Snap a tool event's coordinates (called by `canvas::tool_event` before the tools see it).
pub fn filter_event(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) -> ToolEvent {
    match ev {
        ToolEvent::Down { x, y, pressure } => {
            begin(app, [x, y]);
            // Moves snap their delta, which is zero at the start: keep the press point.
            let p = match app.prefs_rt.snap.as_ref().map(|s| &s.gesture) {
                Some(Gesture::Point) => {
                    let q = apply(app, [x, y]);
                    pixel_round(app, q)
                }
                _ => [x, y],
            };
            ToolEvent::Down { x: p[0], y: p[1], pressure }
        }
        ToolEvent::Move { x, y, pressure } => {
            if app.prefs_rt.snap.is_none() {
                return ev;
            }
            if let Some(s) = app.prefs_rt.snap.as_mut() {
                s.disabled = override_held(mods);
            }
            let q = apply(app, [x, y]);
            let p = pixel_round(app, q);
            ToolEvent::Move { x: p[0], y: p[1], pressure }
        }
        ToolEvent::Up { x, y } => {
            if app.prefs_rt.snap.is_none() {
                return ev;
            }
            if let Some(s) = app.prefs_rt.snap.as_mut() {
                s.disabled = override_held(mods);
            }
            let q = apply(app, [x, y]);
            let p = pixel_round(app, q);
            app.prefs_rt.snap = None;
            app.prefs_rt.snap_lines.clear();
            ToolEvent::Up { x: p[0], y: p[1] }
        }
    }
}

/// Snap a guide being dragged out of a ruler (`vertical` guides snap their x).
pub fn snap_guide(app: &mut PhotocraftApp, vertical: bool, pos: f64) -> f64 {
    if !snap_on(app) {
        return pos;
    }
    let Some(st) = app.session.active() else { return pos };
    let key = (st.doc.id, st.revision);
    if app.prefs_rt.guide_targets.as_ref().is_none_or(|(k, _)| *k != key) {
        let t = build(app, &[], false).filtered(|k| k != SnapKind::Guide);
        app.prefs_rt.guide_targets = Some((key, t));
    }
    let tol = tolerance(app);
    let t = app.prefs_rt.guide_targets.as_ref().map(|(_, t)| t);
    t.and_then(|t| t.nearest(vertical, pos, tol)).map_or(pos, |t| t.pos)
}

fn hex_color(s: &str, fallback: Color32) -> Color32 {
    photocraft_engine::prefs::parse_hex(s).map_or(fallback, |c| Color32::from_rgb(c[0], c[1], c[2]))
}

/// Draw smart guides (and the moving bounds) for the drag in progress.
pub fn draw(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    let Some(snap) = &app.prefs_rt.snap else { return };
    let smart_color = hex_color(&app.session.prefs().guides_grid_and_slices.smart_guide_color, Color32::from_rgb(255, 0, 255));
    // The moved layers' bounds follow the Move tool (Free Transform draws its own box).
    if let (Gesture::Move { rect }, Some(d)) =
        (&snap.gesture, app.drag.as_ref().and_then(|d| d.points.last()).map(|p| [p[0] - snap.start[0], p[1] - snap.start[1]]))
    {
        let r = egui::Rect::from_two_pos(
            xf.to_screen((rect[0] + d[0]) as f32, (rect[1] + d[1]) as f32),
            xf.to_screen((rect[2] + d[0]) as f32, (rect[3] + d[1]) as f32),
        );
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, crate::theme::Tokens::get(painter.ctx()).accent), egui::StrokeKind::Middle);
    }
    if !smart_on(app) {
        return;
    }
    for l in &app.prefs_rt.snap_lines {
        if !l.kind.is_smart() {
            continue;
        }
        draw_line(painter, xf, l, smart_color);
    }
}

fn draw_line(painter: &egui::Painter, xf: &ViewXform, l: &SnapLine, color: Color32) {
    let (a, b) = if l.vertical {
        (xf.to_screen(l.pos as f32, l.from as f32), xf.to_screen(l.pos as f32, l.to as f32))
    } else {
        (xf.to_screen(l.from as f32, l.pos as f32), xf.to_screen(l.to as f32, l.pos as f32))
    };
    painter.line_segment([pos2(a.x.round() + 0.5, a.y.round() + 0.5), pos2(b.x.round() + 0.5, b.y.round() + 0.5)], Stroke::new(1.0, color));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app_with_box() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
        app.sync_views();
        app.ui.views[0].zoom = 1.0;
        app.ui.views[0].fit_pending = false;
        // Two boxes: a target at (300..350, 200..260) and the moving one at (40..90, 40..80).
        app.run("layer.new.layer", json!({"name": "target"})).unwrap();
        app.run("select.rect", json!({"x": 300, "y": 200, "width": 50, "height": 60})).unwrap();
        app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
        app.run("layer.new.layer", json!({"name": "mover"})).unwrap();
        app.run("select.rect", json!({"x": 40, "y": 40, "width": 50, "height": 40})).unwrap();
        app.run("edit.fill", json!({"color": "#0000ff"})).unwrap();
        app.run("select.deselect", json!({})).unwrap();
        app
    }

    fn mover_bounds(app: &PhotocraftApp) -> photocraft_geom::Rect {
        let st = app.session.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds()
    }

    #[test]
    fn move_tool_snaps_to_other_layer_edges_with_smart_guides() {
        let mut app = app_with_box();
        app.ui.tool = Tool::Move;
        let m = egui::Modifiers::NONE;
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 60.0, y: 60.0, pressure: 1.0 }, m);
        // Drag so the mover's left edge lands 3 px right of the target's (x = 303).
        crate::canvas::tool_event(&mut app, ToolEvent::Move { x: 323.0, y: 100.0, pressure: 1.0 }, m);
        assert!(app.prefs_rt.snap_lines.iter().any(|l| l.vertical && l.pos == 300.0 && l.kind.is_smart()), "{:?}", app.prefs_rt.snap_lines);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 323.0, y: 100.0 }, m);
        assert_eq!(mover_bounds(&app).x0, 300);
        assert!(app.prefs_rt.snap.is_none());
    }

    #[test]
    fn ctrl_or_snap_off_disables_snapping() {
        let mut app = app_with_box();
        app.ui.tool = Tool::Move;
        app.ui.view.show.smart_guides = false;
        app.ui.extras.snap = false;
        // The presses land beside the moved layer: keep moving it (no Auto-Select pick).
        app.ui.tool_options.move_auto_select = false;
        let m = egui::Modifiers::NONE;
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 60.0, y: 60.0, pressure: 1.0 }, m);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 323.0, y: 100.0 }, m);
        assert_eq!(mover_bounds(&app).x0, 303);
        app.ui.extras.snap = true;
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 60.0, y: 60.0, pressure: 1.0 }, m);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 62.0, y: 60.0 }, ctrl);
        assert_eq!(mover_bounds(&app).x0, 305, "no snap to the target's right edge at 350? (moved freely)");
        // Ctrl only at the press (Ctrl-click auto-select) doesn't turn snapping off for the drag.
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 60.0, y: 60.0, pressure: 1.0 }, ctrl);
        crate::canvas::tool_event(&mut app, ToolEvent::Move { x: 61.0, y: 60.0, pressure: 1.0 }, m);
        assert!(!app.prefs_rt.snap.as_ref().is_some_and(|s| s.disabled));
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 61.0, y: 60.0 }, m);
        // ⌘ on the Mac is not the override.
        let cmd = egui::Modifiers { mac_cmd: true, command: true, ..Default::default() };
        assert!(!override_held(cmd) && override_held(ctrl));
    }

    #[test]
    fn marquee_corner_snaps_to_guides_and_document_edges() {
        let mut app = app_with_box();
        app.run("view.newGuide", json!({"orientation": "vertical", "position": 120})).unwrap();
        app.ui.tool = Tool::RectMarquee;
        let m = egui::Modifiers::NONE;
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 3.0, y: 2.0, pressure: 1.0 }, m);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 116.0, y: 50.0 }, m);
        let sel = app.session.active().unwrap().doc.selection.as_ref().unwrap().content_bounds();
        assert_eq!((sel.x0, sel.y0, sel.x1), (0, 0, 120));
        // Zoomed in to 800%, 4 px is beyond the 1 px threshold: no snap.
        app.ui.views[0].zoom = 8.0;
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 10.0, pressure: 1.0 }, m);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 116.0, y: 50.0 }, m);
        let sel = app.session.active().unwrap().doc.selection.as_ref().unwrap().content_bounds();
        assert_eq!(sel.x1, 116);
    }

    #[test]
    fn free_transform_drags_and_handles_snap() {
        let mut app = app_with_box();
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, "edit.freeTransform", json!({})).unwrap();
        assert!(app.ui.transform.is_some());
        let m = egui::Modifiers::NONE;
        // Drag from inside the box (40..90 × 40..80) so its left edge lands 2 px from x = 300.
        // (Away from the centre, which holds the reference point.)
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 50.0, y: 50.0, pressure: 1.0 }, m);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 312.0, y: 50.0 }, m);
        let q = app.ui.transform.as_ref().unwrap().quad;
        assert!((q[0][0] - 300.0).abs() < 1e-6, "{q:?}");
        // The right-edge handle snaps to the target's right edge (x = 350).
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: q[1][0], y: (q[1][1] + q[2][1]) / 2.0, pressure: 1.0 }, m);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 353.0, y: 60.0 }, m);
        let q = app.ui.transform.as_ref().unwrap().quad;
        assert!((q[1][0] - 350.0).abs() < 1e-6, "{q:?}");
        crate::transform_tool::commit(&mut app);
        assert_eq!(mover_bounds(&app).x0, 300);
    }

    #[test]
    fn guides_snap_to_layer_edges() {
        let mut app = app_with_box();
        assert_eq!(snap_guide(&mut app, true, 297.0), 300.0);
        assert_eq!(snap_guide(&mut app, false, 150.0), 150.0, "document centre");
        assert_eq!(snap_guide(&mut app, false, 170.0), 170.0);
        app.ui.extras.snap = false;
        assert_eq!(snap_guide(&mut app, true, 297.0), 297.0);
    }

    #[test]
    fn grid_snapping_uses_preferences() {
        let mut app = app_with_box();
        app.ui.extras.grid = true;
        app.run(
            "prefs.set",
            json!({"values": {"guidesGridAndSlices.gridlineEvery": 100, "guidesGridAndSlices.gridUnit": "pixels", "guidesGridAndSlices.subdivisions": 4}}),
        )
        .unwrap();
        assert_eq!(options(&app).grid_step, 25.0);
        app.ui.tool = Tool::RectMarquee;
        let m = egui::Modifiers::NONE;
        crate::canvas::tool_event(&mut app, ToolEvent::Down { x: 152.0, y: 127.0, pressure: 1.0 }, m);
        crate::canvas::tool_event(&mut app, ToolEvent::Up { x: 199.0, y: 176.0 }, m);
        let sel = app.session.active().unwrap().doc.selection.as_ref().unwrap().content_bounds();
        assert_eq!((sel.x0, sel.y0, sel.x1, sel.y1), (150, 125, 200, 175));
    }
}
