//! Edit › Puppet Warp on the canvas: click on the mesh to add pins, drag pins to move them,
//! ⌥-click a pin to remove it; the options bar sets mode, density, expansion, mesh visibility,
//! the selected pin's depth and rotation. ↩ / Commit runs `edit.puppetWarp` with the pins.
//!
//! The preview is the layer texture on the deformed triangle mesh (an egui textured mesh, drawn
//! by the GPU), re-solved with a few warm-started ARAP rounds per drag event (well under a
//! millisecond with the banded Cholesky factor). When a drag ends the mesh is solved from scratch
//! exactly like the engine, so what you see at rest is what Commit produces.

use std::sync::Arc;

use egui::{Color32, Stroke, TextureHandle, pos2, vec2};
use photocraft_algo::puppet::{ITERATIONS, PuppetDensity, PuppetMesh, PuppetMode, PuppetPin, PuppetSolver, PuppetWarp, build_mesh};
use photocraft_doc::{Document, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, ViewXform};

pub struct PuppetSession {
    pub key: u64,
    pub layer: LayerId,
    /// Command to commit with (`edit.puppetWarp` or `layer.smartObjects.puppetWarp`).
    command: String,
    surface: Surface,
    bounds: Rect,
    pub warp: PuppetWarp,
    pub show_mesh: bool,
    pub selected: Option<usize>,
    drag: Option<usize>,
    mesh: PuppetMesh,
    solver: Option<PuppetSolver>,
    pub deformed: Vec<[f64; 2]>,
    order: Vec<usize>,
    texture: TextureHandle,
    opacity: f32,
    pub preview_doc: Arc<Document>,
}

impl PuppetSession {
    pub fn describe(&self) -> Value {
        json!({
            "layer": self.layer.0,
            "pins": self.warp.pins,
            "mode": self.warp.mode,
            "density": self.warp.density,
            "expansion": self.warp.expansion,
            "vertices": self.mesh.verts.len(),
            "triangles": self.mesh.tris.len(),
            "selected": self.selected,
        })
    }

    fn rebuild_mesh(&mut self) {
        self.mesh = build_mesh(&self.surface, self.bounds, self.warp.density, self.warp.expansion);
        self.rebind();
    }

    /// New solver for the current pin set (pins added/removed, rotation fixed or freed).
    fn rebind(&mut self) {
        let src: Vec<[f64; 2]> = self.warp.pins.iter().map(|p| p.src).collect();
        let fixed: Vec<bool> = self.warp.pins.iter().map(|p| p.rotate.is_some()).collect();
        self.solver = Some(PuppetSolver::new(self.mesh.clone(), &src, &fixed));
        self.solve(true);
    }

    /// Solves the deformation: exactly like the engine (`exact`), or a few warm rounds.
    fn solve(&mut self, exact: bool) {
        let Some(solver) = &self.solver else { return };
        let dst: Vec<[f64; 2]> = self.warp.pins.iter().map(|p| p.dst).collect();
        let rot: Vec<Option<f64>> = self.warp.pins.iter().map(|p| p.rotate).collect();
        let warm = (!exact && self.deformed.len() == self.mesh.verts.len()).then_some(self.deformed.as_slice());
        let (v, _) = solver.solve(&dst, &rot, self.warp.mode, if exact { ITERATIONS } else { 6 }, warm);
        self.deformed = v;
        let src: Vec<[f64; 2]> = self.warp.pins.iter().map(|p| p.src).collect();
        let depths: Vec<i32> = self.warp.pins.iter().map(|p| p.depth).collect();
        self.order = solver.draw_order(&depths, &src);
    }

    fn pin_at(&self, p: [f64; 2], tol: f64) -> Option<usize> {
        self.warp
            .pins
            .iter()
            .enumerate()
            .filter(|(_, q)| (q.dst[0] - p[0]).hypot(q.dst[1] - p[1]) <= tol)
            .min_by(|a, b| dist(a.1.dst, p).total_cmp(&dist(b.1.dst, p)))
            .map(|(i, _)| i)
    }

    /// The rest position of a point on the deformed mesh (where a new pin attaches).
    fn rest_of(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        for &t in self.order.iter().rev() {
            let tri = self.mesh.tris[t];
            let [a, b, c] = tri.map(|i| self.deformed[i]);
            let det = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if det.abs() < 1e-12 {
                continue;
            }
            let l1 = ((p[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (p[1] - a[1])) / det;
            let l2 = ((b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1])) / det;
            let l0 = 1.0 - l1 - l2;
            if l0 >= 0.0 && l1 >= 0.0 && l2 >= 0.0 {
                let [ra, rb, rc] = tri.map(|i| self.mesh.verts[i]);
                return Some([l0 * ra[0] + l1 * rb[0] + l2 * rc[0], l0 * ra[1] + l1 * rb[1] + l2 * rc[1]]);
            }
        }
        None
    }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Starts Puppet Warp on the active layer.
pub fn begin(app: &mut PhotocraftApp, ctx: &egui::Context, command: &str) -> Result<(), String> {
    if app.ui.transform.is_some() {
        return Err("finish Free Transform first".into());
    }
    let (layer, surface, _) = crate::distort_ui::active_pixels(app)?;
    let st = app.session.active().ok_or("no document")?;
    let l = st.doc.layer(layer).ok_or("no layer")?;
    let bounds = surface.content_bounds();
    if bounds.is_empty() {
        return Err("Could not use Puppet Warp: the layer is empty".into());
    }
    let opacity = l.opacity * l.fill_opacity;
    let preview_doc = crate::distort_ui::without_layer(&st.doc, layer);
    let img = crate::distort_ui::surface_image(&surface, bounds, 2048);
    let key = app.ui.alloc_id();
    let texture = ctx.load_texture(format!("puppet-{key}"), img, egui::TextureOptions::LINEAR);
    let mut s = PuppetSession {
        key,
        layer,
        command: command.to_string(),
        surface,
        bounds,
        warp: PuppetWarp { pins: Vec::new(), mode: PuppetMode::Normal, density: PuppetDensity::Normal, expansion: 2.0 },
        show_mesh: true,
        selected: None,
        drag: None,
        mesh: PuppetMesh::default(),
        solver: None,
        deformed: Vec::new(),
        order: Vec::new(),
        texture,
        opacity,
        preview_doc,
    };
    s.rebuild_mesh();
    if s.mesh.tris.is_empty() {
        return Err("Could not use Puppet Warp: no opaque pixels".into());
    }
    app.distort.puppet = Some(s);
    Ok(())
}

pub fn commit(app: &mut PhotocraftApp) {
    let Some(s) = app.distort.puppet.take() else { return };
    if s.warp.is_identity() {
        return;
    }
    let mut p = serde_json::to_value(&s.warp).unwrap_or_default();
    p["layer"] = json!(s.layer.0);
    let _ = app.run(&s.command, p);
}

/// Control channel: `edit.puppetWarp {"ui": {...}}` while the mode is active.
pub fn control(app: &mut PhotocraftApp, ui: &Value) -> Result<Value, String> {
    if ui.get("commit").and_then(Value::as_bool) == Some(true) {
        commit(app);
        return Ok(json!({"committed": true}));
    }
    if ui.get("cancel").and_then(Value::as_bool) == Some(true) {
        app.distort.puppet = None;
        return Ok(json!({"cancelled": true}));
    }
    let s = app.distort.puppet.as_mut().ok_or(tl!("Puppet Warp is not active"))?;
    let mut remesh = false;
    if let Some(m) = ui.get("mode").and_then(Value::as_str) {
        s.warp.mode = PuppetMode::parse(m).ok_or("mode: rigid|normal|distort")?;
    }
    if let Some(d) = ui.get("density").and_then(Value::as_str) {
        s.warp.density = PuppetDensity::parse(d).ok_or("density: fewer|normal|more")?;
        remesh = true;
    }
    if let Some(e) = ui.get("expansion").and_then(Value::as_f64) {
        s.warp.expansion = e.clamp(-200.0, 200.0);
        remesh = true;
    }
    if let Some(v) = ui.get("showMesh").and_then(Value::as_bool) {
        s.show_mesh = v;
    }
    if remesh {
        s.rebuild_mesh();
    } else {
        s.solve(true);
    }
    Ok(s.describe())
}

pub fn pointer(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) {
    let tol = crate::distort_ui::tolerance(app);
    let Some(s) = app.distort.puppet.as_mut() else { return };
    match ev {
        ToolEvent::Down { x, y, .. } => {
            let p = [x, y];
            if let Some(i) = s.pin_at(p, tol) {
                if mods.alt {
                    s.warp.pins.remove(i);
                    s.selected = None;
                    s.rebind();
                } else {
                    s.selected = Some(i);
                    s.drag = Some(i);
                }
            } else if !mods.alt
                && let Some(src) = s.rest_of(p)
            {
                s.warp.pins.push(PuppetPin { src, dst: p, rotate: None, depth: 0 });
                s.selected = Some(s.warp.pins.len() - 1);
                s.drag = s.selected;
                s.rebind();
            }
        }
        ToolEvent::Move { x, y, .. } => {
            if let Some(i) = s.drag
                && let Some(pin) = s.warp.pins.get_mut(i)
                && pin.dst != [x, y]
            {
                pin.dst = [x, y];
                s.solve(false);
            }
        }
        ToolEvent::Up { x, y } => {
            if let Some(i) = s.drag.take() {
                if let Some(pin) = s.warp.pins.get_mut(i) {
                    pin.dst = [x, y];
                }
                s.solve(true);
            }
        }
    }
}

pub fn draw(s: &PuppetSession, painter: &egui::Painter, xf: &ViewXform) {
    let b = s.bounds;
    let (w, h) = (f64::from(b.width()), f64::from(b.height()));
    let mut mesh = egui::Mesh::with_texture(s.texture.id());
    let tint = Color32::from_white_alpha((s.opacity.clamp(0.0, 1.0) * 255.0) as u8);
    for (d, r) in s.deformed.iter().zip(&s.mesh.verts) {
        let uv = pos2(((r[0] - f64::from(b.x0)) / w) as f32, ((r[1] - f64::from(b.y0)) / h) as f32);
        mesh.vertices.push(egui::epaint::Vertex { pos: xf.to_screen(d[0] as f32, d[1] as f32), uv, color: tint });
    }
    for &t in &s.order {
        let [a, bb, c] = s.mesh.tris[t];
        mesh.add_triangle(a as u32, bb as u32, c as u32);
    }
    painter.add(mesh);
    if s.show_mesh {
        let line = Stroke::new(0.6, Color32::from_rgba_unmultiplied(40, 40, 40, 150));
        let mut seen = std::collections::HashSet::new();
        for tri in &s.mesh.tris {
            for k in 0..3 {
                let (i, j) = (tri[k].min(tri[(k + 1) % 3]), tri[k].max(tri[(k + 1) % 3]));
                if seen.insert((i, j)) {
                    let (p, q) = (s.deformed[i], s.deformed[j]);
                    painter.line_segment([xf.to_screen(p[0] as f32, p[1] as f32), xf.to_screen(q[0] as f32, q[1] as f32)], line);
                }
            }
        }
    }
    // Pins: yellow discs with a black ring; the selected pin has a black centre (Photoshop).
    for (i, p) in s.warp.pins.iter().enumerate() {
        let c = xf.to_screen(p.dst[0] as f32, p.dst[1] as f32);
        painter.circle_filled(c, 5.5, Color32::from_rgb(255, 214, 10));
        painter.circle_stroke(c, 5.5, Stroke::new(1.25, Color32::BLACK));
        if s.selected == Some(i) {
            painter.circle_filled(c, 2.0, Color32::BLACK);
            if let Some(r) = p.rotate {
                let a = (r as f32).to_radians();
                painter.circle_stroke(c, 18.0, Stroke::new(1.0, Color32::WHITE));
                painter.line_segment([c, c + vec2(a.cos(), a.sin()) * 18.0], Stroke::new(1.0, Color32::WHITE));
            }
        }
    }
}

pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(s) = app.distort.puppet.as_mut() else { return };
    let mut remesh = false;
    let mut resolve = false;
    let mut rebind = false;
    ui.label(tl!("Mode:"));
    let mode = s.warp.mode;
    crate::widgets::dropdown(
        ui,
        "puppet-mode",
        &mut s.warp.mode,
        &[(PuppetMode::Rigid, tl!("Rigid")), (PuppetMode::Normal, tl!("Normal")), (PuppetMode::Distort, tl!("Distort"))],
        90.0,
    );
    resolve |= mode != s.warp.mode;
    ui.label(tl!("Density:"));
    let density = s.warp.density;
    crate::widgets::dropdown(
        ui,
        "puppet-density",
        &mut s.warp.density,
        &[(PuppetDensity::Fewer, tl!("Fewer Points")), (PuppetDensity::Normal, tl!("Normal")), (PuppetDensity::More, tl!("More Points"))],
        110.0,
    );
    remesh |= density != s.warp.density;
    ui.label(tl!("Expansion:"));
    let mut e = s.warp.expansion as f32;
    if crate::widgets::value_field(ui, &mut e, -50.0..=100.0, "px", 56.0).changed() {
        s.warp.expansion = f64::from(e);
        remesh = true;
    }
    crate::widgets::checkbox(ui, &mut s.show_mesh, tl!("Show Mesh"));
    crate::widgets::vline(ui, 22.0);
    if let Some(i) = s.selected.filter(|i| *i < s.warp.pins.len()) {
        ui.label(tl!("Pin Depth:"));
        if ui.small_button("+").on_hover_text(tl!("Bring the selected pin forward")).clicked() {
            s.warp.pins[i].depth += 1;
            resolve = true;
        }
        if ui.small_button("−").on_hover_text(tl!("Send the selected pin backward")).clicked() {
            s.warp.pins[i].depth -= 1;
            resolve = true;
        }
        ui.label(tl!("Rotate:"));
        let mut fixed = s.warp.pins[i].rotate.is_some();
        let was = fixed;
        crate::widgets::dropdown(ui, "puppet-rotate", &mut fixed, &[(false, tl!("Auto")), (true, tl!("Fixed"))], 70.0);
        if fixed != was {
            s.warp.pins[i].rotate = fixed.then_some(0.0);
            rebind = true;
        }
        if let Some(r) = s.warp.pins[i].rotate {
            let mut a = r as f32;
            if crate::widgets::value_field(ui, &mut a, -360.0..=360.0, "°", 52.0).changed() {
                s.warp.pins[i].rotate = Some(f64::from(a));
                resolve = true;
            }
        }
        crate::widgets::vline(ui, 22.0);
    }
    if ui.button(tl!("Remove All Pins")).clicked() {
        s.warp.pins.clear();
        s.selected = None;
        rebind = true;
    }
    if remesh {
        s.rebuild_mesh();
    } else if rebind {
        s.rebind();
    } else if resolve {
        s.solve(true);
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if crate::widgets::primary_button(ui, "✓", 32.0)
            .on_hover_text(crate::i18n::fmt(tl!("Commit Puppet Warp ({key})"), &[("key", &crate::shortcuts::pretty("Enter"))]))
            .clicked()
        {
            commit(app);
        } else if crate::widgets::secondary_button(ui, "⊘", 32.0).on_hover_text(tl!("Cancel (Esc)")).clicked() {
            app.distort.puppet = None;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_drag_and_commit_through_the_engine() {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", json!({"width": 100, "height": 80, "depth": 8})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.session
            .edit("paint", |doc, active| {
                doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(20, 30, 80, 50), &[1.0, 0.0, 0.0, 1.0]);
                Ok(())
            })
            .unwrap();
        app.sync_views();
        crate::distort_ui::menu(&mut app, &ctx, "edit.puppetWarp", &json!({})).unwrap().unwrap();
        let ev = |app: &mut PhotocraftApp, e| crate::distort_ui::pointer(app, e, egui::Modifiers::NONE);
        // Two pins: hold the left end, drag the right end up.
        ev(&mut app, ToolEvent::Down { x: 25.0, y: 40.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 25.0, y: 40.0 });
        ev(&mut app, ToolEvent::Down { x: 75.0, y: 40.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Move { x: 75.0, y: 30.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 75.0, y: 20.0 });
        let s = app.distort.puppet.as_ref().unwrap();
        assert_eq!(s.warp.pins.len(), 2);
        assert_eq!(s.warp.pins[1].dst, [75.0, 20.0]);
        // ⌥-click removes; clicking off the mesh adds nothing.
        ev(&mut app, ToolEvent::Down { x: 5.0, y: 5.0, pressure: 1.0 });
        assert_eq!(app.distort.puppet.as_ref().unwrap().warp.pins.len(), 2);
        crate::distort_ui::pointer(&mut app, ToolEvent::Down { x: 25.0, y: 40.0, pressure: 1.0 }, egui::Modifiers { alt: true, ..Default::default() });
        assert_eq!(app.distort.puppet.as_ref().unwrap().warp.pins.len(), 1);
        ev(&mut app, ToolEvent::Down { x: 25.0, y: 40.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 25.0, y: 40.0 });
        control(&mut app, &json!({"mode": "rigid", "density": "more"})).unwrap();
        let expected = app.distort.puppet.as_ref().unwrap().deformed.clone();
        assert!(!expected.is_empty());
        let before = app.session.active().unwrap().history.past_len();
        crate::distort_ui::menu(&mut app, &ctx, "edit.puppetWarp", &json!({"ui": {"commit": true}})).unwrap().unwrap();
        assert!(app.distort.puppet.is_none());
        assert_eq!(app.session.active().unwrap().history.past_len(), before + 1);
        let st = app.session.active().unwrap();
        let b = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds();
        assert!(b.y0 < 25, "the right end went up: {b:?}");
    }
}
