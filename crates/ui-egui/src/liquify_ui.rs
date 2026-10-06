//! Filter › Liquify…: a full-window dialog like Photoshop's (tool strip on the left, properties on
//! the right, preview in the middle, OK / Cancel).
//!
//! The preview is a CPU proxy: the layer box-downsampled once to at most 1600 px
//! (`ProxyImage`), re-rendered through the displacement field only where a dab changed it
//! (bilinear field and source), and uploaded with a partial texture update. A dab plus its
//! update costs well under a millisecond on a 24 MP layer (see `bench_liquify`). Strokes are
//! recorded exactly as the engine replays them (`stroke_begin` + `stroke_segment` per point), so
//! OK runs `filter.liquify` with the same strokes and gets the same field.

use egui::{Align2, Color32, FontId, Pos2, Rect as ERect, Sense, Stroke, TextureHandle, pos2, vec2};
use photocraft_algo::liquify::{LiquifyField, LiquifyStroke, LiquifyTool, ProxyImage, auto_cell};
use photocraft_doc::LayerId;
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ToolEvent;
use crate::theme::Tokens;
use crate::widgets;

/// Longest side of the preview proxy (px).
const PROXY_SIDE: usize = 1600;
const LEFT_W: f32 = 48.0;
const RIGHT_W: f32 = 292.0;

/// Options shown in the properties panel (and settable over the control channel).
#[derive(Clone, Debug)]
pub struct LiquifyOpts {
    pub tool: LiquifyTool,
    pub size: f32,
    pub density: f32,
    pub pressure: f32,
    pub rate: f32,
    pub show_mesh: bool,
    /// 0 small, 1 medium, 2 large.
    pub mesh_size: usize,
    pub show_mask: bool,
    pub show_backdrop: bool,
    pub backdrop_opacity: f32,
    pub reconstruct_amount: f32,
}

impl Default for LiquifyOpts {
    fn default() -> Self {
        LiquifyOpts {
            tool: LiquifyTool::ForwardWarp,
            size: 100.0,
            density: 50.0,
            pressure: 100.0,
            rate: 80.0,
            show_mesh: false,
            mesh_size: 1,
            show_mask: true,
            show_backdrop: false,
            backdrop_opacity: 50.0,
            reconstruct_amount: 100.0,
        }
    }
}

pub struct LiquifyDialog {
    pub layer: LayerId,
    layer_name: String,
    canvas: Rect,
    cell: f64,
    pub field: LiquifyField,
    pub strokes: Vec<LiquifyStroke>,
    /// The stroke being drawn and its last point.
    cur: Option<(LiquifyStroke, [f64; 3])>,
    proxy: ProxyImage,
    out: Vec<[u8; 4]>,
    tex: Option<TextureHandle>,
    /// Proxy pixels to re-render and upload (x0, y0, x1, y1).
    dirty: Option<[usize; 4]>,
    backdrop: Option<TextureHandle>,
    mask_tex: Option<TextureHandle>,
    mask_dirty: bool,
    pub opts: LiquifyOpts,
    /// Screen px per document px (0 = fit on next frame) and the view centre (document px).
    zoom: f32,
    center: [f64; 2],
    hover: Option<[f64; 2]>,
    last_dab: f64,
    /// Last dab + proxy update time (ms), shown in the footer.
    pub dab_ms: f64,
}

fn union(a: Option<[usize; 4]>, b: [usize; 4]) -> Option<[usize; 4]> {
    if b[0] >= b[2] || b[1] >= b[3] {
        return a;
    }
    Some(match a {
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
        None => b,
    })
}

impl LiquifyDialog {
    pub fn describe(&self) -> Value {
        json!({
            "layer": self.layer.0,
            "tool": self.opts.tool,
            "size": self.opts.size,
            "strokes": self.strokes.len() + usize::from(self.cur.is_some()),
            "meshSize": self.cell,
            "maxDisplacement": self.field.max_displacement(),
            "proxy": [self.proxy.w, self.proxy.h],
            "dabMs": self.dab_ms,
        })
    }

    fn template(&self) -> LiquifyStroke {
        let o = &self.opts;
        LiquifyStroke {
            tool: o.tool,
            size: f64::from(o.size),
            density: f64::from(o.density),
            pressure: f64::from(o.pressure),
            rate: f64::from(o.rate),
            points: Vec::new(),
            amount: None,
        }
    }

    fn mark(&mut self, doc_rect: Rect, mask: bool) {
        if doc_rect.is_empty() {
            return;
        }
        self.dirty = union(self.dirty, self.proxy.proxy_rect(doc_rect));
        self.mask_dirty |= mask;
    }

    fn begin(&mut self, p: [f64; 3], now: f64) {
        let mut s = self.template();
        s.points.push(p.to_vec());
        let t0 = crate::gpu_canvas::now_ms();
        let d = self.field.stroke_begin(&s, p);
        self.mark(d, matches!(s.tool, LiquifyTool::Freeze | LiquifyTool::Thaw));
        self.render_dirty();
        self.dab_ms = crate::gpu_canvas::now_ms() - t0;
        self.cur = Some((s, p));
        self.last_dab = now;
    }

    fn extend(&mut self, p: [f64; 3], now: f64) {
        let Some((mut s, last)) = self.cur.take() else { return };
        let t0 = crate::gpu_canvas::now_ms();
        let d = self.field.stroke_segment(&s, last, p);
        s.points.push(p.to_vec());
        self.mark(d, matches!(s.tool, LiquifyTool::Freeze | LiquifyTool::Thaw));
        self.render_dirty();
        self.dab_ms = crate::gpu_canvas::now_ms() - t0;
        self.cur = Some((s, p));
        self.last_dab = now;
    }

    fn end(&mut self) {
        if let Some((s, _)) = self.cur.take() {
            self.strokes.push(s);
        }
    }

    /// Applies a whole-field operation (Reconstruct…, mask buttons) as a stroke.
    fn global(&mut self, tool: LiquifyTool, amount: Option<f64>) {
        self.end();
        let mut s = LiquifyStroke::new(tool, 1.0);
        s.amount = amount;
        let d = self.field.apply_stroke(&s);
        self.strokes.push(s);
        self.mark(d, true);
        self.render_dirty();
    }

    fn rebuild(&mut self) {
        self.field = LiquifyField::from_strokes(self.canvas, self.cell, &self.strokes);
        self.dirty = Some([0, 0, self.proxy.w, self.proxy.h]);
        self.mask_dirty = true;
        self.render_dirty();
    }

    fn undo(&mut self) {
        self.end();
        if self.strokes.pop().is_some() {
            self.rebuild();
        }
    }

    fn restore_all(&mut self) {
        self.cur = None;
        self.strokes.clear();
        self.rebuild();
    }

    /// Re-renders the dirty proxy pixels into `out` (the texture upload happens in `show`).
    fn render_dirty(&mut self) {
        if let Some(r) = self.dirty {
            self.proxy.render(&self.field, r, &mut self.out);
        }
    }

    fn upload(&mut self, ctx: &egui::Context) {
        let (w, h) = (self.proxy.w, self.proxy.h);
        match (&mut self.tex, self.dirty.take()) {
            (None, _) => {
                self.proxy.render(&self.field, [0, 0, w, h], &mut self.out);
                let img = egui::ColorImage::new([w, h], self.out.iter().map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3])).collect());
                self.tex = Some(ctx.load_texture("liquify-proxy", img, egui::TextureOptions::LINEAR));
            }
            (Some(tex), Some(r)) => {
                let (x0, y0, x1, y1) = (r[0].min(w), r[1].min(h), r[2].min(w), r[3].min(h));
                if x1 > x0 && y1 > y0 {
                    let mut px = Vec::with_capacity((x1 - x0) * (y1 - y0));
                    for y in y0..y1 {
                        px.extend(self.out[y * w + x0..y * w + x1].iter().map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3])));
                    }
                    tex.set_partial([x0, y0], egui::ColorImage::new([x1 - x0, y1 - y0], px), egui::TextureOptions::LINEAR);
                }
            }
            _ => {}
        }
        if self.mask_dirty || self.mask_tex.is_none() {
            self.mask_dirty = false;
            let f = &self.field;
            let img = egui::ColorImage::new(
                [f.w, f.h],
                f.freeze.iter().map(|v| Color32::from_rgba_unmultiplied(230, 40, 40, (v.clamp(0.0, 1.0) * 140.0) as u8)).collect(),
            );
            match &mut self.mask_tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => self.mask_tex = Some(ctx.load_texture("liquify-mask", img, egui::TextureOptions::LINEAR)),
            }
        }
    }
}

/// Opens the dialog on the active layer.
pub fn open(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    let (layer, surf, _) = crate::distort_ui::active_pixels(app)?;
    let st = app.session.active().ok_or("no document")?;
    let canvas = st.doc.bounds();
    let name = st.doc.layer(layer).map(|l| l.name.clone()).unwrap_or_default();
    let cell = auto_cell(canvas);
    let proxy = ProxyImage::new(&surf, canvas, PROXY_SIDE);
    let out = vec![[0u8; 4]; proxy.w * proxy.h];
    let center = [f64::from(canvas.x0 + canvas.x1) / 2.0, f64::from(canvas.y0 + canvas.y1) / 2.0];
    let mut d = LiquifyDialog {
        layer,
        layer_name: name,
        canvas,
        cell,
        field: LiquifyField::new(canvas, cell),
        strokes: Vec::new(),
        cur: None,
        proxy,
        out,
        tex: None,
        dirty: None,
        backdrop: None,
        mask_tex: None,
        mask_dirty: true,
        opts: LiquifyOpts::default(),
        zoom: 0.0,
        center,
        hover: None,
        last_dab: 0.0,
        dab_ms: 0.0,
    };
    // A sensible starting brush: about a tenth of the image.
    d.opts.size = ((canvas.width().max(canvas.height()) as f32) / 10.0).round().clamp(10.0, 1500.0);
    d.upload(ctx);
    app.distort.liquify = Some(d);
    Ok(())
}

/// OK: runs `filter.liquify` with the recorded strokes (one history step).
pub fn commit(app: &mut PhotocraftApp) {
    let Some(mut d) = app.distort.liquify.take() else { return };
    d.end();
    if d.strokes.is_empty() {
        return;
    }
    let p = json!({"strokes": d.strokes, "meshSize": d.cell, "layer": d.layer.0});
    let _ = app.run("filter.liquify", p);
}

/// Control channel: `filter.liquify {"ui": {...}}` while the dialog is open.
pub fn control(app: &mut PhotocraftApp, ui: &Value) -> Result<Value, String> {
    if ui.get("commit").and_then(Value::as_bool) == Some(true) {
        commit(app);
        return Ok(json!({"committed": true}));
    }
    if ui.get("cancel").and_then(Value::as_bool) == Some(true) {
        app.distort.liquify = None;
        return Ok(json!({"cancelled": true}));
    }
    let d = app.distort.liquify.as_mut().ok_or(tl!("Liquify is not open"))?;
    if let Some(t) = ui.get("tool") {
        d.opts.tool = serde_json::from_value(t.clone()).map_err(|e| format!("bad tool: {e}"))?;
    }
    let num = |k: &str| ui.get(k).and_then(Value::as_f64).map(|v| v as f32);
    let flag = |k: &str| ui.get(k).and_then(Value::as_bool);
    if let Some(v) = num("size") {
        d.opts.size = v.clamp(1.0, 15000.0);
    }
    if let Some(v) = num("density") {
        d.opts.density = v.clamp(0.0, 100.0);
    }
    if let Some(v) = num("pressure") {
        d.opts.pressure = v.clamp(0.0, 100.0);
    }
    if let Some(v) = num("rate") {
        d.opts.rate = v.clamp(0.0, 100.0);
    }
    if let Some(v) = flag("showMesh") {
        d.opts.show_mesh = v;
    }
    if let Some(v) = flag("showMask") {
        d.opts.show_mask = v;
    }
    if let Some(v) = flag("showBackdrop") {
        d.opts.show_backdrop = v;
    }
    if let Some(v) = ui.get("meshSize").and_then(Value::as_str) {
        d.opts.mesh_size = match v {
            "small" => 0,
            "large" => 2,
            _ => 1,
        };
    }
    if flag("undo") == Some(true) {
        d.undo();
    }
    if flag("restoreAll") == Some(true) {
        d.restore_all();
    }
    if let Some(a) = num("reconstruct") {
        d.global(LiquifyTool::ReconstructAll, Some(f64::from(a)));
    }
    for (k, t) in [("maskAll", LiquifyTool::FreezeAll), ("maskNone", LiquifyTool::ThawAll), ("invertMask", LiquifyTool::InvertFreeze)] {
        if flag(k) == Some(true) {
            d.global(t, None);
        }
    }
    Ok(d.describe())
}

/// Pointer in document coordinates (from the preview or the control channel).
pub fn pointer(app: &mut PhotocraftApp, ev: ToolEvent, _mods: egui::Modifiers) {
    let now = crate::gpu_canvas::now_ms();
    let Some(d) = app.distort.liquify.as_mut() else { return };
    match ev {
        ToolEvent::Down { x, y, pressure } => d.begin([x, y, f64::from(pressure)], now),
        ToolEvent::Move { x, y, pressure } => {
            d.hover = Some([x, y]);
            if d.cur.is_some() {
                d.extend([x, y, f64::from(pressure)], now);
            }
        }
        ToolEvent::Up { x, y } => {
            if let Some((_, last)) = d.cur
                && (last[0] != x || last[1] != y)
            {
                d.extend([x, y, last[2]], now);
            }
            d.end();
        }
    }
}

pub fn keys(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(d) = app.distort.liquify.as_mut() else { return };
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)) {
        d.undo();
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::OpenBracket)) {
        d.opts.size = (d.opts.size * 0.9).max(1.0);
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::CloseBracket)) {
        d.opts.size = (d.opts.size * 1.1).min(15000.0);
    }
    let tools = [
        (egui::Key::W, LiquifyTool::ForwardWarp),
        (egui::Key::R, LiquifyTool::Reconstruct),
        (egui::Key::E, LiquifyTool::Smooth),
        (egui::Key::C, LiquifyTool::TwirlCw),
        (egui::Key::S, LiquifyTool::Pucker),
        (egui::Key::B, LiquifyTool::Bloat),
        (egui::Key::O, LiquifyTool::PushLeft),
        (egui::Key::F, LiquifyTool::Freeze),
        (egui::Key::D, LiquifyTool::Thaw),
    ];
    for (k, t) in tools {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k)) {
            d.opts.tool = t;
        }
    }
}

fn tool_icon(t: LiquifyTool) -> &'static str {
    match t {
        LiquifyTool::ForwardWarp => "pointer",
        LiquifyTool::Reconstruct => "brush-cleaning",
        LiquifyTool::Smooth => "droplet",
        LiquifyTool::TwirlCw => "rotate-cw",
        LiquifyTool::TwirlCcw => "undo-2",
        LiquifyTool::Pucker => "scan",
        LiquifyTool::Bloat => "maximize-2",
        LiquifyTool::PushLeft => "chevrons-left",
        LiquifyTool::Freeze => "lock",
        LiquifyTool::Thaw => "lock-open",
        _ => "circle",
    }
}

fn shortcut(t: LiquifyTool) -> &'static str {
    match t {
        LiquifyTool::ForwardWarp => "W",
        LiquifyTool::Reconstruct => "R",
        LiquifyTool::Smooth => "E",
        LiquifyTool::TwirlCw => "C",
        LiquifyTool::Pucker => "S",
        LiquifyTool::Bloat => "B",
        LiquifyTool::PushLeft => "O",
        LiquifyTool::Freeze => "F",
        LiquifyTool::Thaw => "D",
        _ => "",
    }
}

fn load_backdrop(app: &PhotocraftApp, ctx: &egui::Context, layer: LayerId, canvas: Rect) -> Option<TextureHandle> {
    let st = app.session.active()?;
    let doc = crate::distort_ui::without_layer(&st.doc, layer);
    let buf = photocraft_compose::render(&doc, canvas);
    let mut s = photocraft_raster::Surface::new(photocraft_color::PixelFormat::RGBA8);
    let flat: Vec<f32> = buf.px.iter().flat_map(|p| *p).collect();
    s.write_region(canvas, &flat);
    Some(ctx.load_texture("liquify-backdrop", crate::distort_ui::surface_image(&s, canvas, PROXY_SIDE), egui::TextureOptions::LINEAR))
}

/// Draws the dialog (a full-window layer over the app).
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    if app.distort.liquify.as_ref().is_some_and(|d| d.opts.show_backdrop && d.backdrop.is_none()) {
        let tex = app.distort.liquify.as_ref().map(|d| (d.layer, d.canvas)).and_then(|(layer, canvas)| load_backdrop(app, ctx, layer, canvas));
        if let Some(d) = app.distort.liquify.as_mut() {
            d.backdrop = tex;
        }
    }
    let mut action: Option<&str> = None;
    let mut events: Vec<ToolEvent> = Vec::new();
    egui::Area::new(egui::Id::new("liquify-dialog")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let Some(d) = app.distort.liquify.as_mut() else { return };
        d.upload(ctx);
        let (full, _) = ui.allocate_exact_size(screen.size(), Sense::hover());
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0.0, t.chrome);
        // Title strip.
        let title = ERect::from_min_size(full.min, vec2(full.width(), 30.0));
        painter.rect_filled(title, 0.0, t.dock);
        painter.line_segment([title.left_bottom(), title.right_bottom()], Stroke::new(1.0, t.separator));
        let pct = if d.zoom > 0.0 { d.zoom * 100.0 } else { 100.0 };
        painter.text(title.center(), Align2::CENTER_CENTER, format!("Liquify ({}, {:.0}%)", d.layer_name, pct), FontId::proportional(13.0), t.text);
        let body = ERect::from_min_max(pos2(full.left(), title.bottom()), full.max);
        // Left tool strip.
        let left = ERect::from_min_size(body.min, vec2(LEFT_W, body.height()));
        painter.rect_filled(left, 0.0, t.dock);
        painter.line_segment([left.right_top(), left.right_bottom()], Stroke::new(1.0, t.separator));
        let mut strip = ui.new_child(egui::UiBuilder::new().max_rect(left.shrink2(vec2(6.0, 8.0))));
        strip.spacing_mut().item_spacing.y = 4.0;
        for tool in LiquifyTool::ALL {
            let tip = format!("{} ({})", tl!(tool.label()), shortcut(tool));
            if crate::icons::button(&mut strip, tool_icon(tool), 34.0, d.opts.tool == tool, &tip).clicked() {
                d.opts.tool = tool;
            }
            if matches!(tool, LiquifyTool::Smooth | LiquifyTool::PushLeft) {
                strip.add_space(6.0);
            }
        }
        // Right properties panel.
        let right = ERect::from_min_size(pos2(body.right() - RIGHT_W, body.top()), vec2(RIGHT_W, body.height()));
        painter.rect_filled(right, 0.0, t.dock);
        painter.line_segment([right.left_top(), right.left_bottom()], Stroke::new(1.0, t.separator));
        let mut props = ui.new_child(egui::UiBuilder::new().max_rect(right.shrink2(vec2(14.0, 12.0))));
        egui::ScrollArea::vertical().id_salt("liquify-props").show(&mut props, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            widgets::section_label(ui, tl!("Brush Tool Options"));
            widgets::slider_row(ui, tl!("Size"), &mut d.opts.size, 1.0..=3000.0, "", None);
            widgets::slider_row(ui, tl!("Density"), &mut d.opts.density, 0.0..=100.0, "", None);
            widgets::slider_row(ui, tl!("Pressure"), &mut d.opts.pressure, 0.0..=100.0, "", None);
            widgets::slider_row(ui, tl!("Rate"), &mut d.opts.rate, 0.0..=100.0, "", None);
            widgets::hairline(ui);
            widgets::section_label(ui, tl!("Brush Reconstruct Options"));
            widgets::slider_row(ui, tl!("Amount"), &mut d.opts.reconstruct_amount, 0.0..=100.0, "%", None);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, tl!("Reconstruct"), 110.0).clicked() {
                    let a = f64::from(d.opts.reconstruct_amount);
                    d.global(LiquifyTool::ReconstructAll, Some(a));
                }
                if widgets::secondary_button(ui, tl!("Restore All"), 110.0).clicked() {
                    d.restore_all();
                }
            });
            widgets::hairline(ui);
            widgets::section_label(ui, tl!("Mask Options"));
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, tl!("None"), 70.0).clicked() {
                    d.global(LiquifyTool::ThawAll, None);
                }
                if widgets::secondary_button(ui, tl!("Mask All"), 80.0).clicked() {
                    d.global(LiquifyTool::FreezeAll, None);
                }
                if widgets::secondary_button(ui, tl!("Invert All"), 80.0).clicked() {
                    d.global(LiquifyTool::InvertFreeze, None);
                }
            });
            widgets::hairline(ui);
            widgets::section_label(ui, tl!("View Options"));
            widgets::checkbox(ui, &mut d.opts.show_mesh, tl!("Show Mesh"));
            ui.horizontal(|ui| {
                ui.label(tl!("Mesh Size"));
                widgets::dropdown(ui, "liquify-mesh-size", &mut d.opts.mesh_size, &[(0, tl!("Small")), (1, tl!("Medium")), (2, tl!("Large"))], 110.0);
            });
            widgets::checkbox(ui, &mut d.opts.show_mask, tl!("Show Mask"));
            widgets::checkbox(ui, &mut d.opts.show_backdrop, tl!("Show Backdrop"));
            if d.opts.show_backdrop {
                widgets::slider_row(ui, tl!("Opacity"), &mut d.opts.backdrop_opacity, 0.0..=100.0, "", None);
            }
            widgets::hairline(ui);
            ui.label(
                egui::RichText::new(format!("{} stroke(s) · field {} px/node · dab {:.2} ms", d.strokes.len(), d.cell, d.dab_ms))
                    .color(t.text_faint)
                    .size(11.0),
            );
        });
        // Footer buttons.
        let foot = ERect::from_min_size(pos2(right.left() + 14.0, right.bottom() - 48.0), vec2(RIGHT_W - 28.0, 32.0));
        let mut fb = ui.new_child(egui::UiBuilder::new().max_rect(foot).layout(egui::Layout::right_to_left(egui::Align::Center)));
        if widgets::primary_button(&mut fb, tl!("OK"), 84.0).clicked() {
            action = Some("ok");
        }
        if widgets::secondary_button(&mut fb, tl!("Cancel"), 84.0).clicked() {
            action = Some("cancel");
        }
        // Preview.
        let area = ERect::from_min_max(pos2(left.right(), body.top()), pos2(right.left(), body.bottom()));
        painter.rect_filled(area, 0.0, t.canvas);
        let (cw, ch) = (d.canvas.width() as f32, d.canvas.height() as f32);
        if d.zoom <= 0.0 {
            d.zoom = ((area.width() - 40.0) / cw).min((area.height() - 40.0) / ch).clamp(0.01, 4.0);
            d.center = [f64::from(d.canvas.x0) + f64::from(cw) / 2.0, f64::from(d.canvas.y0) + f64::from(ch) / 2.0];
        }
        let resp = ui.interact(area, egui::Id::new("liquify-preview"), Sense::click_and_drag());
        // Wheel zooms about the pointer.
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0
                && let Some(hp) = resp.hover_pos()
            {
                let before = to_doc(d, area, hp);
                d.zoom = (d.zoom * (scroll / 200.0).exp()).clamp(0.01, 32.0);
                let after = to_doc(d, area, hp);
                d.center = [d.center[0] + before[0] - after[0], d.center[1] + before[1] - after[1]];
            }
        }
        let img = ERect::from_min_max(
            to_screen(d, area, [f64::from(d.canvas.x0), f64::from(d.canvas.y0)]),
            to_screen(d, area, [f64::from(d.canvas.x1), f64::from(d.canvas.y1)]),
        );
        let clip = painter.with_clip_rect(area);
        widgets::checker(&clip, img.intersect(area), 8.0);
        let uv = ERect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        if d.opts.show_backdrop
            && let Some(b) = &d.backdrop
        {
            clip.image(b.id(), img, uv, Color32::from_white_alpha((d.opts.backdrop_opacity / 100.0 * 255.0) as u8));
        }
        // The proxy covers whole proxy pixels, a little past the canvas edge.
        let pr =
            ERect::from_min_max(img.min, img.min + vec2(d.proxy.w as f32 * d.proxy.scale as f32 * d.zoom, d.proxy.h as f32 * d.proxy.scale as f32 * d.zoom));
        if let Some(tex) = &d.tex {
            clip.image(tex.id(), pr, uv, Color32::WHITE);
        }
        if d.opts.show_mask
            && let Some(m) = &d.mask_tex
        {
            let f = &d.field;
            let mr = ERect::from_min_size(
                img.min - vec2(0.5, 0.5) * d.zoom * f.cell as f32 + vec2(0.5, 0.5) * d.zoom,
                vec2(f.w as f32, f.h as f32) * f.cell as f32 * d.zoom,
            );
            clip.image(m.id(), mr, uv, Color32::WHITE);
        }
        if d.opts.show_mesh {
            draw_mesh(d, &clip, area);
        }
        // Brush outline.
        if let Some(hp) = resp.hover_pos() {
            let r = d.opts.size * 0.5 * d.zoom;
            clip.circle_stroke(hp, r, Stroke::new(1.5, Color32::BLACK));
            clip.circle_stroke(hp, r, Stroke::new(0.75, Color32::WHITE));
            d.hover = Some(to_doc(d, area, hp));
        }
        clip.rect_stroke(img, 0.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Outside);
        // Mouse → strokes (Space or middle button pans).
        let pan = ui.input(|i| i.key_down(egui::Key::Space) || i.pointer.middle_down());
        if pan && resp.dragged() {
            let dl = resp.drag_delta();
            d.center = [d.center[0] - f64::from(dl.x / d.zoom), d.center[1] - f64::from(dl.y / d.zoom)];
        } else if let Some(pp) = resp.interact_pointer_pos() {
            let q = to_doc(d, area, pp);
            let pr = 1.0;
            if resp.drag_started() || (resp.is_pointer_button_down_on() && d.cur.is_none()) {
                events.push(ToolEvent::Down { x: q[0], y: q[1], pressure: pr });
            } else if resp.dragged() || resp.is_pointer_button_down_on() {
                let moved = d.cur.as_ref().is_some_and(|(_, l)| l[0] != q[0] || l[1] != q[1]);
                // Stationary tools keep working while the brush is held still (30 dabs/s).
                let held = d.opts.tool.is_stationary() && crate::gpu_canvas::now_ms() - d.last_dab > 33.0;
                if moved || held {
                    events.push(ToolEvent::Move { x: q[0], y: q[1], pressure: pr });
                }
                ctx.request_repaint();
            }
        }
        if resp.drag_stopped() || (d.cur.is_some() && !ui.input(|i| i.pointer.primary_down())) {
            let q = d.cur.as_ref().map(|(_, l)| [l[0], l[1]]).unwrap_or_default();
            events.push(ToolEvent::Up { x: q[0], y: q[1] });
        }
    });
    for ev in events {
        pointer(app, ev, egui::Modifiers::NONE);
    }
    match action {
        Some("ok") => commit(app),
        Some("cancel") => app.distort.liquify = None,
        _ => {}
    }
}

fn to_screen(d: &LiquifyDialog, area: ERect, p: [f64; 2]) -> Pos2 {
    area.center() + vec2(((p[0] - d.center[0]) as f32) * d.zoom, ((p[1] - d.center[1]) as f32) * d.zoom)
}

fn to_doc(d: &LiquifyDialog, area: ERect, p: Pos2) -> [f64; 2] {
    let v = (p - area.center()) / d.zoom;
    [d.center[0] + f64::from(v.x), d.center[1] + f64::from(v.y)]
}

/// The deformed mesh: where the source grid lines land. Each output sample knows its source
/// position `p + d(p)`, so a source line `x = c` is the iso-line of that function, traced with
/// marching squares on a sample grid four times finer than the mesh (exact for any field, unlike
/// inverting the field point by point, which fails where it folds).
fn draw_mesh(d: &LiquifyDialog, painter: &egui::Painter, area: ERect) {
    let c = d.canvas;
    let cells = [64.0, 40.0, 24.0][d.opts.mesh_size.min(2)];
    let step = f64::from(c.width().max(c.height())) / cells;
    let h = step / 4.0;
    let nx = (f64::from(c.width()) / h).ceil() as usize + 1;
    let ny = (f64::from(c.height()) / h).ceil() as usize + 1;
    let (x0, y0) = (f64::from(c.x0), f64::from(c.y0));
    let mut src = Vec::with_capacity(nx * ny);
    for j in 0..ny {
        for i in 0..nx {
            let p = [x0 + i as f64 * h, y0 + j as f64 * h];
            let v = d.field.sample(p[0], p[1]);
            src.push([p[0] + v[0], p[1] + v[1]]);
        }
    }
    let stroke = Stroke::new(0.75, Color32::from_rgba_unmultiplied(90, 90, 90, 210));
    let pos = |i: usize, j: usize| [x0 + i as f64 * h, y0 + j as f64 * h];
    for axis in [0usize, 1] {
        let origin = if axis == 0 { x0 } else { y0 };
        for jj in 0..ny - 1 {
            for ii in 0..nx - 1 {
                let corners = [(ii, jj), (ii + 1, jj), (ii + 1, jj + 1), (ii, jj + 1)];
                let vals = corners.map(|(i, j)| src[j * nx + i][axis]);
                let (lo, hi) = (vals.iter().copied().fold(f64::MAX, f64::min), vals.iter().copied().fold(f64::MIN, f64::max));
                let mut k = ((lo - origin) / step).ceil() as i64;
                while origin + k as f64 * step <= hi {
                    let level = origin + k as f64 * step;
                    let mut pts: Vec<[f64; 2]> = Vec::with_capacity(4);
                    for e in 0..4 {
                        let (a, b) = (vals[e] - level, vals[(e + 1) % 4] - level);
                        if (a <= 0.0) != (b <= 0.0) {
                            let t = a / (a - b);
                            let (pa, pb) = (pos(corners[e].0, corners[e].1), pos(corners[(e + 1) % 4].0, corners[(e + 1) % 4].1));
                            pts.push([pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t]);
                        }
                    }
                    for seg in pts.as_chunks::<2>().0 {
                        painter.line_segment([to_screen(d, area, seg[0]), to_screen(d, area, seg[1])], stroke);
                    }
                    k += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_layer() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", json!({"width": 120, "height": 80, "depth": 8})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.session
            .edit("paint", |doc, active| {
                let s = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
                for y in 10..70 {
                    for x in 10..110 {
                        let v = ((x / 5 + y / 5) % 2) as f32;
                        s.write_pixel(x, y, &[v, 0.3, 1.0 - v, 1.0]);
                    }
                }
                Ok(())
            })
            .unwrap();
        app.sync_views();
        app
    }

    #[test]
    fn dialog_strokes_match_the_engine_and_commit_once() {
        let ctx = egui::Context::default();
        let mut app = app_with_layer();
        open(&mut app, &ctx).unwrap();
        control(&mut app, &json!({"tool": "forwardWarp", "size": 40})).unwrap();
        for ev in [
            ToolEvent::Down { x: 40.0, y: 40.0, pressure: 1.0 },
            ToolEvent::Move { x: 48.0, y: 41.0, pressure: 1.0 },
            ToolEvent::Move { x: 56.0, y: 40.0, pressure: 1.0 },
            ToolEvent::Up { x: 56.0, y: 40.0 },
        ] {
            pointer(&mut app, ev, egui::Modifiers::NONE);
        }
        control(&mut app, &json!({"tool": "bloat"})).unwrap();
        for ev in [ToolEvent::Down { x: 80.0, y: 40.0, pressure: 1.0 }, ToolEvent::Move { x: 80.0, y: 40.0, pressure: 1.0 }, ToolEvent::Up { x: 80.0, y: 40.0 }]
        {
            pointer(&mut app, ev, egui::Modifiers::NONE);
        }
        let d = app.distort.liquify.as_ref().unwrap();
        assert_eq!(d.strokes.len(), 2);
        // Replaying the recorded strokes gives exactly the interactive field.
        let replay = LiquifyField::from_strokes(d.canvas, d.cell, &d.strokes);
        assert_eq!(replay, d.field);
        assert!(d.field.max_displacement() > 3.0);
        // Undo inside the dialog drops the last stroke.
        control(&mut app, &json!({"undo": true})).unwrap();
        assert_eq!(app.distort.liquify.as_ref().unwrap().strokes.len(), 1);
        let before = app.session.active().unwrap().history.past_len();
        control(&mut app, &json!({"commit": true})).unwrap();
        assert!(app.distort.liquify.is_none());
        assert_eq!(app.session.active().unwrap().history.past_len(), before + 1);
    }

    #[test]
    fn undo_rerenders_the_proxy_before_the_next_texture_upload() {
        let ctx = egui::Context::default();
        let mut app = app_with_layer();
        open(&mut app, &ctx).unwrap();
        control(&mut app, &json!({"tool": "forwardWarp", "size": 40})).unwrap();
        for ev in [
            ToolEvent::Down { x: 35.0, y: 40.0, pressure: 1.0 },
            ToolEvent::Move { x: 55.0, y: 40.0, pressure: 1.0 },
            ToolEvent::Up { x: 55.0, y: 40.0 },
            ToolEvent::Down { x: 70.0, y: 40.0, pressure: 1.0 },
            ToolEvent::Move { x: 90.0, y: 40.0, pressure: 1.0 },
            ToolEvent::Up { x: 90.0, y: 40.0 },
        ] {
            pointer(&mut app, ev, egui::Modifiers::NONE);
        }

        let before = app.distort.liquify.as_ref().unwrap().out.clone();
        control(&mut app, &json!({"undo": true})).unwrap();
        let d = app.distort.liquify.as_ref().unwrap();
        assert_eq!(d.strokes.len(), 1);
        assert_ne!(d.out, before, "undo must update the pixels shown by the existing texture");

        let mut expected = vec![[0u8; 4]; d.proxy.w * d.proxy.h];
        d.proxy.render(&d.field, [0, 0, d.proxy.w, d.proxy.h], &mut expected);
        assert_eq!(d.out, expected, "dirty upload source must match the rebuilt field");
    }

    #[test]
    fn shortcut_handler_undoes_liquify_even_when_egui_owns_keyboard_focus() {
        let ctx = egui::Context::default();
        let mut app = app_with_layer();
        open(&mut app, &ctx).unwrap();
        for ev in [ToolEvent::Down { x: 40.0, y: 40.0, pressure: 1.0 }, ToolEvent::Move { x: 52.0, y: 40.0, pressure: 1.0 }, ToolEvent::Up { x: 52.0, y: 40.0 }]
        {
            pointer(&mut app, ev, egui::Modifiers::NONE);
        }
        assert_eq!(app.distort.liquify.as_ref().unwrap().strokes.len(), 1);

        let raw = egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx();
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("liquify-slider-focus")));
            assert!(ctx.egui_wants_keyboard_input());
            crate::shortcuts::handle(&mut app, ctx);
        });
        out.textures_delta.clear();
        assert_eq!(app.distort.liquify.as_ref().unwrap().strokes.len(), 0);
    }

    #[test]
    fn menu_opens_the_dialog_and_routes_pointer_events() {
        let ctx = egui::Context::default();
        let mut app = app_with_layer();
        let r = crate::distort_ui::menu(&mut app, &ctx, "filter.liquify", &json!({})).unwrap().unwrap();
        assert!(r["liquify"]["proxy"].is_array());
        assert!(crate::distort_ui::pointer(&mut app, ToolEvent::Down { x: 50.0, y: 40.0, pressure: 1.0 }, egui::Modifiers::NONE));
        crate::distort_ui::pointer(&mut app, ToolEvent::Up { x: 60.0, y: 40.0 }, egui::Modifiers::NONE);
        assert_eq!(app.distort.liquify.as_ref().unwrap().strokes.len(), 1);
        control(&mut app, &json!({"cancel": true})).unwrap();
        assert!(app.distort.liquify.is_none());
        assert_eq!(app.session.active().unwrap().history.past_len(), 2, "cancel records nothing");
    }
}
