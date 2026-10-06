//! Vector tools: Shape tools (U), Pen (P), Path Selection (A), path overlays and the Paths panel.
//! All edits go through the engine's `shape.*` / `path.*` commands.

use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use photocraft_doc::vector::Path;
use photocraft_doc::{Document, LayerContent};
use photocraft_geom::Affine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::{Tool, ToolOptions};
use crate::theme::Tokens;

/// Pen tool path under construction: knots as [anchor, in, out] (document px).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PenPath {
    pub knots: Vec<[[f64; 2]; 3]>,
    #[serde(skip)]
    pub dragging: bool,
}

pub fn is_shape_tool(t: Tool) -> bool {
    matches!(t, Tool::Rectangle | Tool::EllipseShape | Tool::Triangle | Tool::Polygon | Tool::Line | Tool::CustomShape)
}

fn rgb32(c: [f32; 4]) -> Color32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(b(c[0]), b(c[1]), b(c[2]))
}

fn hex(c: [f32; 4]) -> String {
    let c = rgb32(c);
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn stroke_param(app: &PhotocraftApp) -> Value {
    let o = &app.ui.tool_options;
    if o.stroke_width > 0.0 { json!({"width": o.stroke_width, "color": hex(app.session.tools.background)}) } else { Value::Null }
}

/// Geometry of a Shape-tool drag as `shape.create` params, without fill and stroke (None when too
/// small): ⇧ constrains proportions, ⌥ draws from the centre. Custom shapes give their box as a `rect`.
fn shape_geometry(o: &ToolOptions, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) -> Option<Value> {
    Some(if tool == Tool::Line {
        let (mut dx, mut dy) = (end[0] - start[0], end[1] - start[1]);
        if mods.shift {
            // Snap to 45°.
            let a = (dy.atan2(dx) / std::f64::consts::FRAC_PI_4).round() * std::f64::consts::FRAC_PI_4;
            let len = dx.hypot(dy);
            (dx, dy) = (len * a.cos(), len * a.sin());
        }
        if dx.hypot(dy) < 1.0 {
            return None;
        }
        json!({"kind": "line", "from": start, "to": [start[0] + dx, start[1] + dy], "weight": o.line_weight.max(1.0)})
    } else {
        let (mut w, mut h) = (end[0] - start[0], end[1] - start[1]);
        if mods.shift {
            let m = w.abs().max(h.abs());
            (w, h) = (m * w.signum(), m * h.signum());
        }
        let (x0, y0) = if mods.alt { (start[0] - w, start[1] - h) } else { (start[0], start[1]) };
        let (w, h) = if mods.alt { (w * 2.0, h * 2.0) } else { (w, h) };
        let rect = [x0.min(x0 + w).round(), y0.min(y0 + h).round(), w.abs().round(), h.abs().round()];
        if rect[2] < 1.0 || rect[3] < 1.0 {
            return None;
        }
        match tool {
            Tool::Rectangle if o.corner_radius > 0.0 => json!({"kind": "roundedRect", "rect": rect, "radii": vec![o.corner_radius; 4]}),
            Tool::Rectangle | Tool::CustomShape => json!({"kind": "rect", "rect": rect}),
            Tool::EllipseShape => json!({"kind": "ellipse", "rect": rect}),
            Tool::Triangle => json!({"kind": "polygon", "rect": rect, "sides": 3}),
            _ => json!({"kind": "polygon", "rect": rect, "sides": o.polygon_sides.max(3)}),
        }
    })
}

/// Finish a Shape-tool drag: ⇧ constrains proportions, ⌥ draws from the centre.
pub fn finish_shape(app: &mut PhotocraftApp, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) {
    let Some(mut p) = shape_geometry(&app.ui.tool_options, tool, start, end, mods) else { return };
    let fill = if app.ui.tool_options.shape_fill { json!(hex(app.session.tools.foreground)) } else { Value::Null };
    let stroke = stroke_param(app);
    if tool == Tool::CustomShape {
        // ⇧ keeps the shape's proportions (the rect is already squared).
        if let Ok(rect) = serde_json::from_value(p["rect"].take()) {
            crate::preset_panels::finish_custom_shape(app, rect, mods.shift, fill, stroke);
        }
        return;
    }
    p["fill"] = fill;
    p["stroke"] = stroke;
    if let Err(e) = app.run("shape.create", p) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Shape-tool drag preview: the shape `finish_shape` will create, filled and stroked, under its
/// path outline. Custom shapes show their box outline only.
pub fn draw_shape_preview(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) {
    let o = &app.ui.tool_options;
    let Some(path) = shape_geometry(o, tool, start, end, mods).and_then(|p| photocraft_engine::vector_cmds::shape_path(&p).ok()) else { return };
    let custom = tool == Tool::CustomShape;
    // ponytail: preview colours skip the canvas's colour management; the commit renders them exactly.
    let fill = if o.shape_fill && !custom { rgb32(app.session.tools.foreground) } else { Color32::TRANSPARENT };
    let stroke = if o.stroke_width > 0.0 && !custom { Stroke::new(o.stroke_width * xf.zoom, rgb32(app.session.tools.background)) } else { Stroke::NONE };
    let accent = Tokens::get(painter.ctx()).accent;
    for (pts, _) in path_lines(&path, &|q| xf.to_screen(q[0] as f32, q[1] as f32)) {
        // Every tool's own shape is convex (custom shapes aren't, and draw no fill).
        painter.add(egui::Shape::convex_polygon(pts.clone(), fill, stroke));
        // The accent path over a dark halo: visible on any pixels (#172).
        painter.extend(crate::tool_feedback::contrast_path(pts, true, accent));
    }
}

// ---------------------------------------------------------------------------------------------
// Pen

fn pen_to_json(pen: &PenPath, closed: bool) -> Value {
    json!({"subpaths": [{"closed": closed, "knots": pen.knots.iter().map(|k| json!({"anchor": k[0], "in": k[1], "out": k[2], "smooth": k[1] != k[0] || k[2] != k[0]})).collect::<Vec<_>>()}]})
}

/// Pen press: close on the first anchor, else add an anchor (dragging pulls smooth handles).
pub fn pen_down(app: &mut PhotocraftApp, x: f64, y: f64) {
    let tol = 6.0 / app.current_zoom().max(0.01) as f64;
    let pen = app.ui.pen.get_or_insert_with(PenPath::default);
    if let Some(first) = pen.knots.first().map(|k| k[0])
        && pen.knots.len() >= 2
        && (first[0] - x).hypot(first[1] - y) < tol
    {
        pen_commit(app, true);
        return;
    }
    pen.knots.push([[x, y]; 3]);
    pen.dragging = true;
}

pub fn pen_move(app: &mut PhotocraftApp, x: f64, y: f64) {
    if let Some(pen) = app.ui.pen.as_mut()
        && pen.dragging
        && let Some(k) = pen.knots.last_mut()
    {
        let a = k[0];
        k[2] = [x, y];
        k[1] = [2.0 * a[0] - x, 2.0 * a[1] - y];
    }
}

pub fn pen_up(app: &mut PhotocraftApp) {
    if let Some(pen) = app.ui.pen.as_mut() {
        pen.dragging = false;
    }
}

/// Finish the pen path: a work path (Path mode) or a new shape layer (Shape mode).
pub fn pen_commit(app: &mut PhotocraftApp, closed: bool) {
    let Some(pen) = app.ui.pen.take() else { return };
    if pen.knots.len() < 2 {
        return;
    }
    let path = pen_to_json(&pen, closed);
    let r = if app.ui.tool_options.vector_mode == "shape" {
        let fill = if closed && app.ui.tool_options.shape_fill { json!(hex(app.session.tools.foreground)) } else { Value::Null };
        let stroke =
            if closed { stroke_param(app) } else { json!({"width": app.ui.tool_options.stroke_width.max(1.0), "color": hex(app.session.tools.foreground)}) };
        app.run("shape.create", json!({"kind": "path", "path": path, "fill": fill, "stroke": stroke}))
    } else if let Some((id, existing)) = targeted_vector_mask(app) {
        // A targeted vector mask takes the new subpath (#196), as in Photoshop.
        let mut p = photocraft_engine::vector_cmds::path_json(&existing);
        if let (Some(subs), Some(new)) = (p.get_mut("subpaths").and_then(Value::as_array_mut), path.get("subpaths").and_then(Value::as_array)) {
            subs.extend(new.iter().cloned());
        }
        app.run("layer.vectorMask.edit", json!({"layer": id, "path": p}))
    } else {
        app.run("path.set", json!({"name": "work", "path": path}))
    };
    if let Err(e) = r {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// The path that path commands act on without a name: the one selected in the Paths panel, else
/// the work path, else the selected layer's shape path or vector mask; `None` when there's none.
pub fn active_path_name(app: &PhotocraftApp) -> Option<String> {
    let st = app.session.active()?;
    let rows = path_rows(&st.doc, st.active_layer);
    let key = |r: &PathEntry| match r.kind {
        PathRow::Work => "work".to_string(),
        PathRow::Layer => "layer".to_string(),
        PathRow::Saved => r.name.clone(),
    };
    let selected = app.ui.selected_path.as_deref().filter(|s| rows.iter().any(|r| key(r) == *s)).map(str::to_string);
    selected.or_else(|| rows.iter().find(|r| r.kind == PathRow::Work).map(key)).or_else(|| rows.iter().find(|r| r.kind == PathRow::Layer).map(key))
}

/// ⌘↩ / Ctrl+Enter (#306): load a path as a selection, like Photoshop with a Pen or Path
/// Selection tool or a path selected in the Paths panel. A path being drawn with the Pen is
/// finished first and loads; otherwise [`active_path_name`]. `params` may set feather, mode…
pub fn path_to_selection(app: &mut PhotocraftApp, params: Value) -> Result<Value, String> {
    if app.ui.pen.as_ref().is_some_and(|p| p.knots.len() >= 2) {
        // The finished path becomes the work path, or a shape layer's path or a vector mask.
        let on_layer = app.ui.tool_options.vector_mode == "shape" || targeted_vector_mask(app).is_some();
        pen_commit(app, false);
        app.ui.selected_path = Some(if on_layer { "layer" } else { "work" }.into());
    }
    let name = active_path_name(app)
        .ok_or_else(|| tl!("No path to make a selection from: draw one with the Pen tool or select one in the Paths panel.").to_string())?;
    let mut p = params.as_object().cloned().unwrap_or_default();
    p.insert("name".into(), json!(name));
    app.run("path.toSelection", Value::Object(p))
}

// ---------------------------------------------------------------------------------------------
// Path Selection

/// What Path Selection edits.
enum PathTarget {
    Shape(u64),
    /// The active layer's vector mask, when its Layers thumbnail is targeted (#196).
    VectorMask(u64),
    Work,
}

/// The targeted vector mask of the active layer, if the Layers panel targets it.
fn targeted_vector_mask(app: &PhotocraftApp) -> Option<(u64, Path)> {
    let st = app.session.active()?;
    let l = st.active_layer.and_then(|id| st.doc.layer(id))?;
    (app.ui.vector_mask_target && !matches!(l.content, LayerContent::Shape(_))).then_some(())?;
    l.vector_mask.as_ref().map(|m| (l.id.0, m.path.clone()))
}

/// The path Path Selection edits: the targeted vector mask, the active shape layer's path, else
/// the work path.
fn target_path(app: &PhotocraftApp) -> Option<(PathTarget, Path)> {
    if let Some((id, p)) = targeted_vector_mask(app) {
        return Some((PathTarget::VectorMask(id), p));
    }
    let st = app.session.active()?;
    if let Some(l) = st.active_layer.and_then(|id| st.doc.layer(id))
        && let LayerContent::Shape(sh) = &l.content
    {
        return Some((PathTarget::Shape(l.id.0), sh.path.clone()));
    }
    st.doc.work_path.clone().map(|p| (PathTarget::Work, p))
}

pub fn path_selection_finish(app: &mut PhotocraftApp, start: [f64; 2], end: [f64; 2]) {
    let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
    if dx.abs() + dy.abs() < 0.5 {
        return;
    }
    let Some((target, path)) = target_path(app) else { return };
    let _ = match target {
        PathTarget::Shape(id) => app.run("shape.edit", json!({"layer": id, "move": [dx.round(), dy.round()]})),
        PathTarget::VectorMask(id) => {
            let moved = path.transform(&Affine::translate(dx.round(), dy.round()));
            app.run("layer.vectorMask.edit", json!({"layer": id, "path": photocraft_engine::vector_cmds::path_json(&moved)}))
        }
        PathTarget::Work => {
            let moved = path.transform(&Affine::translate(dx.round(), dy.round()));
            app.run("path.set", json!({"name": "work", "path": photocraft_engine::vector_cmds::path_json(&moved)}))
        }
    };
}

// ---------------------------------------------------------------------------------------------
// Drawing

fn bezier(p0: [f64; 2], c0: [f64; 2], c1: [f64; 2], p1: [f64; 2], t: f64) -> [f64; 2] {
    let u = 1.0 - t;
    let f = |a: f64, b: f64, c: f64, d: f64| u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d;
    [f(p0[0], c0[0], c1[0], p1[0]), f(p0[1], c0[1], c1[1], p1[1])]
}

/// Screen polylines for a path (one per subpath).
fn path_lines(path: &Path, xf: &dyn Fn([f64; 2]) -> Pos2) -> Vec<(Vec<Pos2>, bool)> {
    path.subpaths
        .iter()
        .map(|s| {
            let n = s.knots.len();
            let mut pts = Vec::new();
            let segs = if s.closed { n } else { n.saturating_sub(1) };
            for i in 0..segs {
                let (a, b) = (&s.knots[i], &s.knots[(i + 1) % n]);
                for k in 0..=16 {
                    let q =
                        bezier([a.anchor.x, a.anchor.y], [a.out_ctrl.x, a.out_ctrl.y], [b.in_ctrl.x, b.in_ctrl.y], [b.anchor.x, b.anchor.y], k as f64 / 16.0);
                    pts.push(xf(q));
                }
            }
            if pts.is_empty()
                && let Some(k) = s.knots.first()
            {
                pts.push(xf([k.anchor.x, k.anchor.y]));
            }
            (pts, s.closed)
        })
        .collect()
}

/// Work path / active shape path outlines, anchors, and the pen path in progress.
pub fn draw_overlay(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, doc: &Document) {
    let tool = app.ui.tool;
    let vector_tool = matches!(tool, Tool::Pen | Tool::PathSelection) || is_shape_tool(tool);
    let accent = Tokens::get(painter.ctx()).accent;
    let to_scr = |q: [f64; 2]| xf.to_screen(q[0] as f32, q[1] as f32);
    let draw_path = |p: &Path, anchors: bool| {
        for (pts, closed) in path_lines(p, &to_scr) {
            if closed {
                painter.add(egui::Shape::closed_line(pts, Stroke::new(1.0, accent)));
            } else {
                painter.add(egui::Shape::line(pts, Stroke::new(1.0, accent)));
            }
        }
        if anchors {
            for s in &p.subpaths {
                for k in &s.knots {
                    let c = to_scr([k.anchor.x, k.anchor.y]);
                    painter.rect_filled(Rect::from_center_size(c, vec2(6.0, 6.0)), 0.0, Color32::WHITE);
                    painter.rect_stroke(Rect::from_center_size(c, vec2(6.0, 6.0)), 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
                }
            }
        }
    };
    if vector_tool {
        if let Some(wp) = &doc.work_path {
            draw_path(wp, tool == Tool::PathSelection);
        }
        let st = app.session.active();
        if let Some(l) = st.and_then(|s| s.active_layer.and_then(|id| s.doc.layer(id)))
            && let LayerContent::Shape(sh) = &l.content
        {
            draw_path(&sh.path, tool == Tool::PathSelection);
        }
    }
    // A targeted vector mask shows its path with any tool (#196).
    if let Some((_, p)) = targeted_vector_mask(app) {
        draw_path(&p, tool == Tool::PathSelection);
    }
    // Pen path in progress, with handles of the last knot and a rubber band to the pointer.
    if let Some(pen) = &app.ui.pen {
        let mut pts = Vec::new();
        for w in pen.knots.windows(2) {
            for k in 0..=16 {
                pts.push(to_scr(bezier(w[0][0], w[0][2], w[1][1], w[1][0], k as f64 / 16.0)));
            }
        }
        painter.add(egui::Shape::line(pts, Stroke::new(1.5, accent)));
        if let (Some(last), Some(h)) = (pen.knots.last(), app.hover_doc)
            && !pen.dragging
        {
            painter.add(egui::Shape::dashed_line(&[to_scr(last[0]), to_scr(h)], Stroke::new(1.0, accent), 4.0, 3.0));
        }
        for (i, k) in pen.knots.iter().enumerate() {
            let c = to_scr(k[0]);
            if i + 1 == pen.knots.len() && k[2] != k[0] {
                for hnd in [k[1], k[2]] {
                    let hp = to_scr(hnd);
                    painter.line_segment([c, hp], Stroke::new(1.0, accent));
                    painter.circle_filled(hp, 3.0, accent);
                }
            }
            let r = Rect::from_center_size(c, vec2(6.0, 6.0));
            painter.rect_filled(r, 0.0, if i + 1 == pen.knots.len() { accent } else { Color32::WHITE });
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Options bars

/// Options bar for vector tools; false for other tools.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) -> bool {
    if !(is_shape_tool(tool) || matches!(tool, Tool::Pen | Tool::PathSelection)) {
        return false;
    }
    let t = Tokens::get(ui.ctx());
    let lbl = |ui: &mut egui::Ui, s: &str| {
        ui.label(egui::RichText::new(s).color(t.text_dim).size(12.0));
    };
    let o = &mut app.ui.tool_options;
    if tool == Tool::PathSelection {
        lbl(ui, if app.ui.vector_mask_target { "Drag to move the targeted vector mask" } else { tl!("Drag to move the active shape's path or the Work Path") });
        return true;
    }
    if tool == Tool::Pen {
        let opts = [("path".to_string(), tl!("Path")), ("shape".to_string(), tl!("Shape"))];
        crate::widgets::dropdown(ui, "pen-mode", &mut o.vector_mode, &opts, 80.0);
        crate::widgets::vline(ui, 22.0);
        lbl(
            ui,
            &crate::i18n::fmt(
                tl!("Click: corner · Drag: smooth · Click first point: close · {key} finish · Esc cancel"),
                &[("key", &crate::shortcuts::pretty("Enter"))],
            ),
        );
        return true;
    }
    let mut mode = "shape".to_string();
    crate::widgets::dropdown(ui, "shape-mode", &mut mode, &[("shape".to_string(), tl!("Shape"))], 80.0);
    crate::widgets::vline(ui, 22.0);
    lbl(ui, tl!("Fill:"));
    crate::widgets::checkbox(ui, &mut o.shape_fill, "");
    let (r, _) = ui.allocate_exact_size(vec2(22.0, 16.0), Sense::hover());
    ui.painter().rect_filled(r, 2.0, rgb32(app.session.tools.foreground));
    ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
    lbl(ui, tl!("Stroke:"));
    crate::widgets::value_field(ui, &mut o.stroke_width, 0.0..=288.0, "px", 58.0);
    match tool {
        Tool::Rectangle => {
            crate::widgets::vline(ui, 22.0);
            lbl(ui, tl!("Radius:"));
            crate::widgets::value_field(ui, &mut o.corner_radius, 0.0..=10000.0, "px", 62.0);
        }
        Tool::Polygon => {
            crate::widgets::vline(ui, 22.0);
            lbl(ui, tl!("Sides:"));
            let mut s = o.polygon_sides as f32;
            if crate::widgets::value_field(ui, &mut s, 3.0..=100.0, "", 48.0).changed() {
                o.polygon_sides = s.round().clamp(3.0, 100.0) as u32;
            }
        }
        Tool::Line => {
            crate::widgets::vline(ui, 22.0);
            lbl(ui, tl!("Weight:"));
            crate::widgets::value_field(ui, &mut o.line_weight, 1.0..=1000.0, "px", 58.0);
        }
        Tool::CustomShape => {
            crate::widgets::vline(ui, 22.0);
            crate::preset_panels::shape_picker(app, ui);
        }
        _ => {}
    }
    true
}

// ---------------------------------------------------------------------------------------------
// Properties (shape layers)

fn color_of(f: &photocraft_doc::Fill) -> Option<Color32> {
    match f {
        photocraft_doc::Fill::Solid(c) => {
            let v = c.to_rgba8();
            Some(Color32::from_rgb(v[0], v[1], v[2]))
        }
        _ => None,
    }
}

/// A colour swatch that opens a picker; returns the new `#rrggbb` when changed.
fn swatch(ui: &mut egui::Ui, fill: Option<&photocraft_doc::Fill>, tip: &str) -> Option<String> {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 18.0), Sense::click());
    let current = fill.and_then(color_of);
    match (current, fill) {
        (Some(c), _) => {
            ui.painter().rect_filled(r, 2.0, c);
        }
        (None, Some(photocraft_doc::Fill::Gradient { stops, .. })) if !stops.is_empty() => {
            // Gradient fills preview as a left-to-right ramp through their stops.
            let rgb = |c: &photocraft_doc::Color| {
                let v = c.to_rgba8();
                Color32::from_rgb(v[0], v[1], v[2])
            };
            let mut mesh = egui::Mesh::default();
            let mut ramp: Vec<(f32, Color32)> = stops.iter().map(|(p, c)| (p.clamp(0.0, 1.0), rgb(c))).collect();
            ramp.insert(0, (0.0, ramp[0].1));
            ramp.push((1.0, ramp[ramp.len() - 1].1));
            for (i, (p, c)) in ramp.iter().enumerate() {
                let x = r.left() + r.width() * p;
                mesh.colored_vertex(egui::pos2(x, r.top()), *c);
                mesh.colored_vertex(egui::pos2(x, r.bottom()), *c);
                if i > 0 {
                    let k = (i as u32) * 2;
                    mesh.add_triangle(k - 2, k - 1, k);
                    mesh.add_triangle(k - 1, k, k + 1);
                }
            }
            ui.painter().add(mesh);
        }
        (None, Some(_)) => {
            // Pattern fill: a neutral checker hint.
            crate::widgets::checker(ui.painter(), r, 4.0);
        }
        (None, None) => {
            // "No colour": white with a red slash, like Photoshop.
            ui.painter().rect_filled(r, 2.0, Color32::WHITE);
            ui.painter().line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.5, Color32::from_rgb(220, 40, 40)));
        }
    };
    ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
    let resp = resp.on_hover_text(tip);
    let mut out = None;
    egui::Popup::from_toggle_button_response(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        let mut c = current.unwrap_or(Color32::BLACK);
        if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
            out = Some(format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b()));
        }
        if fill.is_some() && ui.button(tl!("No Color")).clicked() {
            out = Some("none".into());
        }
    });
    out
}

/// Properties panel for a shape layer: Appearance (fill, stroke) and live shape geometry.
pub fn shape_properties(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: photocraft_doc::LayerId) {
    let Some(sh) = app.session.active().and_then(|s| s.doc.layer(id)).and_then(|l| match &l.content {
        LayerContent::Shape(sh) => Some(sh.clone()),
        _ => None,
    }) else {
        return;
    };
    let t = Tokens::get(ui.ctx());
    let mut edit: Option<Value> = None;
    let key = |k: &str| format!("shape-{}-{k}", id.0);
    // The shared collapsible section headers (#155).
    if crate::props_layout::section(ui, "appearance", tl!("Appearance")) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Fill")).color(t.text_dim).size(12.0));
            if let Some(c) = swatch(ui, sh.fill.as_ref(), tl!("Set shape fill type")) {
                edit = Some(if c == "none" { json!({"fill": null}) } else { json!({"fill": c, "coalesce": key("fill")}) });
            }
            ui.add_space(12.0);
            ui.label(egui::RichText::new(tl!("Stroke")).color(t.text_dim).size(12.0));
            if let Some(c) = swatch(ui, sh.stroke.as_ref().map(|s| &s.paint), tl!("Set shape stroke type")) {
                edit = Some(if c == "none" {
                    json!({"stroke": null})
                } else {
                    json!({"stroke": {"color": c, "width": sh.stroke.as_ref().map_or(3.0, |s| s.width)}, "coalesce": key("stroke")})
                });
            }
            let mut w = sh.stroke.as_ref().map_or(0.0, |s| s.width);
            if crate::widgets::value_field(ui, &mut w, 0.0..=288.0, "px", 60.0).changed() {
                edit = Some(if w <= 0.0 { json!({"stroke": null}) } else { json!({"stroke": {"width": w}, "coalesce": key("stroke-w")}) });
            }
            if sh.stroke.is_some() {
                let mut align = match sh.stroke.as_ref().map(|s| s.align) {
                    Some(photocraft_doc::vector::StrokeAlign::Inside) => "inside",
                    Some(photocraft_doc::vector::StrokeAlign::Outside) => "outside",
                    _ => "center",
                }
                .to_string();
                let opts = [("inside".to_string(), "Inside"), ("center".to_string(), "Center"), ("outside".to_string(), "Outside")];
                if crate::widgets::dropdown(ui, &key("align"), &mut align, &opts, 84.0) {
                    edit = Some(json!({"stroke": {"align": align}}));
                }
            }
        });
    }
    if let Some(live) = &sh.live
        && crate::props_layout::section(ui, "liveShape", tl!("Shape"))
    {
        let num = |ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, unit: &str| -> bool {
            ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim).size(12.0));
            crate::widgets::value_field(ui, v, range, unit, 64.0).changed()
        };
        match live {
            photocraft_doc::vector::LiveShape::Rect { rect, .. }
            | photocraft_doc::vector::LiveShape::Ellipse { rect }
            | photocraft_doc::vector::LiveShape::Polygon { rect, .. } => {
                ui.horizontal(|ui| {
                    let (mut w, mut h) = (rect[2] as f32, rect[3] as f32);
                    let cw = num(ui, "W", &mut w, 1.0..=300000.0, "px");
                    let ch = num(ui, "H", &mut h, 1.0..=300000.0, "px");
                    if cw || ch {
                        edit = Some(json!({"rect": [rect[0], rect[1], w.round(), h.round()], "coalesce": key("wh")}));
                    }
                });
            }
            photocraft_doc::vector::LiveShape::Line { .. } => {}
        }
        ui.horizontal(|ui| match live {
            photocraft_doc::vector::LiveShape::Rect { radii, .. } => {
                let mut r = radii[0] as f32;
                if num(ui, tl!("Corner radius"), &mut r, 0.0..=100000.0, "px") {
                    edit = Some(json!({"radii": r, "coalesce": key("radius")}));
                }
            }
            photocraft_doc::vector::LiveShape::Polygon { sides, star_ratio, .. } => {
                let mut n = *sides as f32;
                if num(ui, tl!("Sides"), &mut n, 3.0..=100.0, "") {
                    edit = Some(json!({"sides": n.round() as u32, "coalesce": key("sides")}));
                }
                let mut sr = (*star_ratio * 100.0) as f32;
                if num(ui, tl!("Star ratio"), &mut sr, 1.0..=100.0, "%") {
                    edit = Some(json!({"starRatio": sr as f64 / 100.0, "coalesce": key("star")}));
                }
            }
            photocraft_doc::vector::LiveShape::Line { weight, .. } => {
                let mut w = *weight as f32;
                if num(ui, tl!("Weight"), &mut w, 1.0..=10000.0, "px") {
                    edit = Some(json!({"weight": w, "coalesce": key("weight")}));
                }
            }
            _ => {}
        });
    }
    if let Some(mut p) = edit {
        p["layer"] = json!(id.0);
        if let Err(e) = app.run("shape.edit", p) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Paths panel

fn thumb(ui: &egui::Ui, r: Rect, path: &Path, doc: &Document) {
    let t = Tokens::get(ui.ctx());
    // At the document's aspect ratio, like the Channels thumbnails.
    let r = crate::channels_panel::fit_thumb(r, doc.size.width, doc.size.height).0;
    ui.painter().rect_filled(r, 0.0, Color32::from_gray(if t.pro { 222 } else { 240 }));
    let (w, h) = (doc.size.width.max(1) as f64, doc.size.height.max(1) as f64);
    let s = (r.width() as f64 / w).min(r.height() as f64 / h);
    let off = r.center() - vec2((w * s) as f32 / 2.0, (h * s) as f32 / 2.0);
    let map = |q: [f64; 2]| pos2(off.x + (q[0] * s) as f32, off.y + (q[1] * s) as f32);
    for (pts, closed) in path_lines(path, &map) {
        if closed && pts.len() >= 3 {
            ui.painter().add(egui::Shape::closed_line(pts, Stroke::new(1.0, Color32::from_gray(40))));
        } else {
            ui.painter().add(egui::Shape::line(pts, Stroke::new(1.0, Color32::from_gray(40))));
        }
    }
}

/// What a Paths panel row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRow {
    Saved,
    Work,
    /// The selected layer's shape path or vector mask (temporary, italic).
    Layer,
}

/// A Paths panel row.
#[derive(Clone, Debug)]
pub struct PathEntry {
    pub name: String,
    pub path: Path,
    pub kind: PathRow,
}

/// The Paths panel's rows, top to bottom: saved paths, the work path, then the selected layer's
/// shape path ("<Layer> Shape Path") or vector mask ("<Layer> Vector Mask"), like Photoshop.
pub fn path_rows(doc: &Document, active: Option<photocraft_doc::LayerId>) -> Vec<PathEntry> {
    let mut rows: Vec<_> = doc.paths.iter().map(|p| PathEntry { name: p.name.clone(), path: p.path.clone(), kind: PathRow::Saved }).collect();
    if let Some(wp) = &doc.work_path {
        rows.push(PathEntry { name: "Work Path".into(), path: wp.clone(), kind: PathRow::Work });
    }
    if let Some(l) = active.and_then(|id| doc.layer(id)) {
        let layer_path = match &l.content {
            LayerContent::Shape(sh) => Some((format!("{} Shape Path", l.name), sh.path.clone())),
            _ => l.vector_mask.as_ref().map(|v| (format!("{} Vector Mask", l.name), v.path.clone())),
        };
        if let Some((name, path)) = layer_path {
            rows.push(PathEntry { name, path, kind: PathRow::Layer });
        }
    }
    rows
}

fn footer_id() -> egui::Id {
    egui::Id::new("paths-footer")
}

fn ctx_data_footer(ctx: &egui::Context, r: Rect) {
    ctx.data_mut(|d| d.insert_temp(footer_id(), r));
}

/// Where the Paths panel's button footer was drawn last frame.
pub fn paths_footer(ctx: &egui::Context) -> Option<Rect> {
    ctx.data(|d| d.get_temp(footer_id()))
}

pub fn paths_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else {
        ui.label(egui::RichText::new(tl!("No document")).color(t.text_faint));
        return;
    };
    let doc = st.doc.clone();
    let rows = path_rows(&doc, st.active_layer);
    let has_layer_path = rows.iter().any(|r| r.kind == PathRow::Layer);
    let mut action: Option<(&str, Value)> = None;
    // The buttons sit in a footer at the panel's bottom, like Photoshop's.
    let footer = 34.0;
    let fill = ui.available_height() > footer + 60.0;
    let rows_h = if fill { ui.available_height() - footer } else { f32::INFINITY };
    egui::ScrollArea::vertical().id_salt("path-rows").max_height(rows_h).min_scrolled_height(if fill { rows_h } else { 0.0 }).auto_shrink([false, !fill]).show(
        ui,
        |ui| {
            if rows.is_empty() {
                ui.label(egui::RichText::new(tl!("Draw with the Pen tool (P) or make a work path from a selection.")).color(t.text_faint).size(11.5));
            }
            for PathEntry { name, path, kind } in &rows {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                let key = match kind {
                    PathRow::Work => "work".to_string(),
                    PathRow::Layer => "layer".to_string(),
                    PathRow::Saved => name.clone(),
                };
                // Rows are painted: name them for screen readers and UI tests.
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::SelectableLabel, true, name));
                let sel = app.ui.selected_path.as_deref() == Some(key.as_str());
                if sel {
                    ui.painter().rect_filled(r, 0.0, t.row_selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.5));
                }
                thumb(ui, Rect::from_min_size(pos2(r.left() + 8.0, r.top() + 4.0), vec2(28.0, 28.0)), path, &doc);
                let mut job = egui::text::LayoutJob::default();
                // Temporary paths (the work path, the selected layer's shape path or vector mask) are italic.
                let italics = *kind != PathRow::Saved;
                job.append(
                    if *kind == PathRow::Work { tl!(name) } else { name },
                    0.0,
                    egui::TextFormat { font_id: egui::FontId::proportional(12.0), color: t.text, italics, ..Default::default() },
                );
                let g = ui.painter().layout_job(job);
                ui.painter().galley(pos2(r.left() + 46.0, r.center().y - g.size().y / 2.0), g, t.text);
                if resp.clicked() {
                    app.ui.selected_path = Some(key.clone());
                }
                // Double-clicking the Work Path saves it (Photoshop's "Save Path").
                if resp.double_clicked() && *kind == PathRow::Work {
                    let n = doc.paths.len() + 1;
                    action = Some(("path.rename", json!({"name": "work", "to": format!("Path {n}")})));
                }
                ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.separator));
            }
        },
    );
    ui.add_space(4.0);
    crate::widgets::hairline(ui);
    ui.add_space(2.0);
    let sel = app.ui.selected_path.clone().filter(|s| s != "layer" || has_layer_path).unwrap_or_else(|| "work".into());
    let footer_rect = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let fg = hex(app.session.tools.foreground);
        let mut n = doc.paths.len() + 1;
        while doc.paths.iter().any(|p| p.name == format!("Path {n}")) {
            n += 1;
        }
        let items: [(&str, &str, &str, Value); 7] = [
            ("paint-bucket", "Fill path with foreground color", "path.fill", json!({"name": sel, "color": fg})),
            ("circle", "Stroke path with brush", "path.stroke", json!({"name": sel, "tool": "brush"})),
            ("square-dashed", "Load path as a selection", "path.toSelection", json!({"name": sel})),
            ("spline", "Make work path from selection", "select.toWorkPath", json!({"tolerance": 2.0})),
            ("square", "Add vector mask", "layer.vectorMask.fromPath", json!({"name": sel})),
            ("square-plus", "Create new path", "path.set", json!({"name": format!("Path {n}"), "path": {"subpaths": []}})),
            ("trash", "Delete path", "path.delete", json!({"name": sel})),
        ];
        // The layer's own path: deleting it deletes the vector mask (a shape keeps its path).
        let shape = rows.iter().any(|r| r.kind == PathRow::Layer && r.name.ends_with(" Shape Path"));
        let delete_layer_path = sel == "layer" && !shape;
        let items = items.map(|(icon, tip, cmd, p)| {
            if cmd == "path.delete" && delete_layer_path { (icon, "Delete vector mask", "layer.vectorMask.delete", json!({})) } else { (icon, tip, cmd, p) }
        });
        for (icon, tip, cmd, p) in items {
            let icon = if crate::icons::exists(icon) { icon } else { "square" };
            if crate::icons::button(ui, icon, 24.0, false, tip).clicked() {
                action = Some((cmd, p));
            }
        }
    });
    ctx_data_footer(ui.ctx(), footer_rect.response.rect);
    if let Some((cmd, p)) = action
        && let Err(e) = app.run(cmd, p)
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 200, "height": 200})).unwrap();
        app.sync_views();
        app
    }

    #[test]
    fn shape_drag_creates_layers_with_modifiers() {
        let mut app = app();
        finish_shape(&mut app, Tool::Rectangle, [10.0, 10.0], [60.0, 30.0], egui::Modifiers::SHIFT);
        let st = app.session.active().unwrap();
        let b = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds();
        assert_eq!((b.width(), b.height()), (50, 50), "shift = square");
        finish_shape(&mut app, Tool::EllipseShape, [100.0, 100.0], [120.0, 110.0], egui::Modifiers::ALT);
        let st = app.session.active().unwrap();
        let b = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds();
        assert!(b.x0.abs_diff(80) <= 1 && b.width().abs_diff(40) <= 1, "alt = from centre: {b:?}");
        finish_shape(&mut app, Tool::Line, [10.0, 150.0], [90.0, 152.0], egui::Modifiers::SHIFT);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 4);
    }

    #[test]
    fn shape_preview_is_the_committed_path() {
        let mut app = app();
        app.ui.tool_options.corner_radius = 8.0;
        let preview = |app: &PhotocraftApp, tool, end| {
            shape_geometry(&app.ui.tool_options, tool, [10.0, 10.0], end, egui::Modifiers::ALT)
                .and_then(|p| photocraft_engine::vector_cmds::shape_path(&p).ok())
        };
        for tool in [Tool::Rectangle, Tool::EllipseShape, Tool::Triangle, Tool::Polygon, Tool::Line] {
            let shown = preview(&app, tool, [60.0, 30.0]).unwrap();
            finish_shape(&mut app, tool, [10.0, 10.0], [60.0, 30.0], egui::Modifiers::ALT);
            let st = app.session.active().unwrap();
            let LayerContent::Shape(sh) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!("{tool:?}: no shape layer") };
            assert_eq!(sh.path, shown, "{tool:?}");
        }
        assert!(preview(&app, Tool::Rectangle, [10.2, 40.0]).is_none(), "too thin to draw");
        assert!(photocraft_engine::vector_cmds::shape_path(&json!({"kind": "nope", "rect": [0, 0, 5, 5]})).is_err());
        assert!(photocraft_engine::vector_cmds::shape_path(&json!({"kind": "rect"})).is_err());
    }

    #[test]
    fn pen_builds_and_closes_a_work_path() {
        let mut app = app();
        app.ui.tool = Tool::Pen;
        for (x, y) in [(20.0, 20.0), (120.0, 20.0)] {
            pen_down(&mut app, x, y);
            pen_up(&mut app);
        }
        pen_down(&mut app, 120.0, 120.0);
        pen_move(&mut app, 140.0, 140.0); // smooth knot
        pen_up(&mut app);
        pen_down(&mut app, 20.5, 20.5); // click the first anchor: close
        assert!(app.ui.pen.is_none());
        let wp = app.session.active().unwrap().doc.work_path.clone().unwrap();
        assert!(wp.subpaths[0].closed && wp.subpaths[0].knots.len() == 3);
        assert!(wp.subpaths[0].knots[2].smooth);
        // Path Selection moves it.
        path_selection_finish(&mut app, [50.0, 50.0], [60.0, 55.0]);
        let wp = app.session.active().unwrap().doc.work_path.clone().unwrap();
        assert_eq!((wp.subpaths[0].knots[0].anchor.x, wp.subpaths[0].knots[0].anchor.y), (30.0, 25.0));
    }
}

#[cfg(test)]
#[path = "path_selection_tests.rs"]
mod path_selection_tests;
