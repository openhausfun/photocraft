//! Filter › Filter Gallery…: a full-window dialog like Photoshop's: a big preview on the left,
//! the category folders with thumbnails in the middle, and on the right OK / Cancel, the
//! selected effect's filter and settings, and the effect-layer stack (new, delete, reorder,
//! show/hide). The stack is applied bottom to top.
//!
//! The preview runs the same algorithm as the command on the visible part of the layer
//! (downsampled when zoomed out). OK runs `filter.filterGallery {effects:[...]}`, one history
//! step (a smart filter on smart objects). Everything is drivable over the control channel with
//! `filter.filterGallery {"ui": {...}}` while the dialog is open.

use std::hash::{Hash, Hasher};

use egui::{Align2, Color32, FontId, Rect as ERect, Sense, Stroke, TextureHandle, pos2, vec2};
use photocraft_algo::{FilterParams, GALLERY_CATEGORIES, GalleryFilter};
use photocraft_doc::LayerId;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::filter_dialog::{Kind, parse_spec};
use crate::theme::Tokens;
use crate::widgets;

const MID_W: f32 = 300.0;
const RIGHT_W: f32 = 290.0;
const THUMB: [usize; 2] = [80, 56];

/// One effect layer of the stack.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectLayer {
    pub filter: GalleryFilter,
    pub params: Map<String, Value>,
    pub visible: bool,
}

impl EffectLayer {
    fn new(filter: GalleryFilter) -> Self {
        EffectLayer { filter, params: default_params(filter), visible: true }
    }
    fn to_json(&self) -> Value {
        json!({"filter": self.filter.key(), "params": self.params, "visible": self.visible})
    }
}

/// The filter's defaults in command-param form (choices by name).
fn default_params(f: GalleryFilter) -> Map<String, Value> {
    let mut m = Map::new();
    for p in parse_spec(f.params_doc()) {
        let v = match p.kind {
            Kind::Range { default, .. } => json!(default),
            Kind::Choice(c) => json!(c[0]),
            Kind::Bool(b) => json!(b),
            _ => continue,
        };
        m.insert(p.key, v);
    }
    m
}

pub struct GalleryDialog {
    pub layer: LayerId,
    layer_name: String,
    canvas: Rect,
    src: Surface,
    /// Apply order: index 0 runs first (shown at the bottom of the list).
    pub effects: Vec<EffectLayer>,
    pub selected: usize,
    open: [bool; 6],
    foreground: [f32; 4],
    background: [f32; 4],
    /// Screen px per document px (0 = fit) and the view centre (document px).
    pub zoom: f32,
    center: [f32; 2],
    tex: Option<TextureHandle>,
    shown: Option<(u64, ERect)>,
    thumb_src: Surface,
    thumbs: Vec<Option<TextureHandle>>,
    pub preview_ms: f64,
}

impl GalleryDialog {
    pub fn describe(&self) -> Value {
        json!({
            "layer": self.layer.0,
            "effects": self.effects.iter().map(EffectLayer::to_json).collect::<Vec<_>>(),
            "selected": self.selected,
            "zoom": if self.zoom > 0.0 { json!(self.zoom) } else { json!("fit") },
            "previewMs": self.preview_ms,
        })
    }

    /// The command params for the current stack.
    pub fn params(&self) -> Value {
        json!({"effects": self.effects.iter().map(EffectLayer::to_json).collect::<Vec<_>>(), "foreground": self.foreground, "background": self.background})
    }

    fn cur(&mut self) -> &mut EffectLayer {
        let i = self.selected.min(self.effects.len() - 1);
        &mut self.effects[i]
    }

    fn set_filter(&mut self, f: GalleryFilter) {
        let e = self.cur();
        if e.filter != f {
            *e = EffectLayer { visible: e.visible, ..EffectLayer::new(f) };
        }
    }

    fn add(&mut self) {
        // Like Photoshop: the new effect layer starts as a copy of the selected one, above it.
        let e = self.cur().clone();
        self.selected = (self.selected + 1).min(self.effects.len());
        self.effects.insert(self.selected, e);
    }

    fn delete(&mut self) {
        if self.effects.len() > 1 {
            self.effects.remove(self.selected.min(self.effects.len() - 1));
            self.selected = self.selected.saturating_sub(1).min(self.effects.len() - 1);
        }
    }

    fn move_to(&mut self, from: usize, to: usize) {
        if from < self.effects.len() && to < self.effects.len() && from != to {
            let e = self.effects.remove(from);
            self.effects.insert(to, e);
            self.selected = to;
        }
    }
}

/// A nearest-neighbour copy of `r` of `surf`, every `k`-th pixel, placed at the origin.
fn sample(surf: &Surface, r: Rect, k: u32) -> Surface {
    let k = k.max(1) as i32;
    let (w, h) = ((r.width() as i32 + k - 1) / k, (r.height() as i32 + k - 1) / k);
    let mut out = Surface::new(surf.format());
    if w <= 0 || h <= 0 {
        return out;
    }
    let n = surf.channels();
    let mut data = Vec::with_capacity((w * h) as usize * n);
    for j in 0..h {
        let y = r.y0 + j * k;
        let row = surf.read_region(Rect::new(r.x0, y, r.x1, y + 1));
        for i in 0..w {
            let o = (i * k) as usize * n;
            data.extend_from_slice(&row[o..o + n]);
        }
    }
    out.write_region(Rect::new(0, 0, w, h), &data);
    out
}

/// Runs a stack on a small surface (the preview and thumbnails).
fn render(src: &Surface, params: &Value) -> Surface {
    let effects = photocraft_engine::gallery_cmds::effects_from_json(params).unwrap_or_default();
    let b = src.content_bounds().union(&Rect::new(0, 0, 1, 1));
    let b = Rect::new(0, 0, b.x1, b.y1);
    photocraft_algo::apply_in(src, &FilterParams::FilterGallery { effects }, b, b, None, b)
}

fn image(surf: &Surface, w: usize, h: usize) -> egui::ColorImage {
    let mut px = vec![[0u8; 4]; w * h];
    for y in 0..h {
        surf.read_rgba8_into(Rect::new(0, y as i32, w as i32, y as i32 + 1), &mut px[y * w..(y + 1) * w]);
    }
    let alpha = surf.format().alpha;
    egui::ColorImage::new([w, h], px.iter().map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], if alpha { p[3] } else { 255 })).collect())
}

/// Opens the dialog on the active layer, starting from the last gallery stack used.
pub fn open(app: &mut PhotocraftApp) -> Result<(), String> {
    let (layer, src, _) = crate::distort_ui::active_pixels(app)?;
    let st = app.session.active().ok_or("no document")?;
    let canvas = st.doc.bounds();
    let layer_name = st.doc.layer(layer).map(|l| l.name.clone()).unwrap_or_default();
    let last = app.session.journal.iter().rev().find(|(id, _)| id == "filter.filterGallery").map(|(_, p)| p.clone());
    let mut effects: Vec<EffectLayer> = last
        .as_ref()
        .and_then(|p| p.get("effects").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            let f = GalleryFilter::from_key(e.get("filter")?.as_str()?)?;
            let mut l = EffectLayer::new(f);
            if let Some(Value::Object(m)) = e.get("params") {
                l.params.extend(m.clone());
            }
            l.visible = e.get("visible").and_then(Value::as_bool).unwrap_or(true);
            Some(l)
        })
        .collect();
    if effects.is_empty() {
        effects.push(EffectLayer::new(GalleryFilter::ColoredPencil));
    }
    // Thumbnail source: the middle of the layer, about 4× the thumbnail size.
    let cb = src.content_bounds().intersect(&canvas);
    let cb = if cb.is_empty() { canvas } else { cb };
    let k = ((cb.width() as usize / (THUMB[0] * 4)).min(cb.height() as usize / (THUMB[1] * 4))).max(1) as u32;
    let (tw, th) = (THUMB[0] as i32 * k as i32, THUMB[1] as i32 * k as i32);
    let (cx, cy) = ((cb.x0 + cb.x1) / 2, (cb.y0 + cb.y1) / 2);
    let thumb_src = sample(&src, Rect::new(cx - tw / 2, cy - th / 2, cx - tw / 2 + tw, cy - th / 2 + th), k);
    let sel = effects.len() - 1;
    let open_cats = std::array::from_fn(|i| effects[sel].filter.category() == GALLERY_CATEGORIES[i]);
    app.distort.gallery = Some(GalleryDialog {
        layer,
        layer_name,
        canvas,
        src,
        effects,
        selected: sel,
        open: open_cats,
        foreground: app.session.tools.foreground,
        background: app.session.tools.background,
        zoom: 0.0,
        center: [(canvas.x0 + canvas.x1) as f32 / 2.0, (canvas.y0 + canvas.y1) as f32 / 2.0],
        tex: None,
        shown: None,
        thumb_src,
        thumbs: vec![None; GalleryFilter::ALL.len()],
        preview_ms: 0.0,
    });
    Ok(())
}

/// OK: runs `filter.filterGallery` with the stack (one history step).
pub fn commit(app: &mut PhotocraftApp) -> Result<Value, String> {
    let Some(d) = app.distort.gallery.take() else { return Err("the Filter Gallery is not open".into()) };
    if !d.effects.iter().any(|e| e.visible) {
        return Ok(json!({"committed": false}));
    }
    let mut p = d.params();
    p["layer"] = json!(d.layer.0);
    app.run("filter.filterGallery", p)
}

/// Control channel: `filter.filterGallery {"ui": {...}}` while the dialog is open.
pub fn control(app: &mut PhotocraftApp, ui: &Value) -> Result<Value, String> {
    let flag = |k: &str| ui.get(k).and_then(Value::as_bool) == Some(true);
    if flag("commit") {
        let r = commit(app)?;
        return Ok(json!({"committed": true, "result": r}));
    }
    if flag("cancel") {
        app.distort.gallery = None;
        return Ok(json!({"cancelled": true}));
    }
    let d = app.distort.gallery.as_mut().ok_or("the Filter Gallery is not open")?;
    if let Some(i) = ui.get("select").and_then(Value::as_u64) {
        d.selected = (i as usize).min(d.effects.len() - 1);
    }
    if flag("add") {
        d.add();
    }
    if flag("delete") {
        d.delete();
    }
    if let Some(i) = ui.get("toggle").and_then(Value::as_u64)
        && let Some(e) = d.effects.get_mut(i as usize)
    {
        e.visible = !e.visible;
    }
    if let Some(m) = ui.get("move") {
        let g = |k: &str| m.get(k).and_then(Value::as_u64).map(|v| v as usize);
        if let (Some(a), Some(b)) = (g("from"), g("to")) {
            d.move_to(a, b);
        }
    }
    if let Some(k) = ui.get("filter").and_then(Value::as_str) {
        let f = GalleryFilter::from_key(k).ok_or_else(|| format!("unknown gallery filter `{k}`"))?;
        d.set_filter(f);
    }
    if let Some(Value::Object(m)) = ui.get("params") {
        d.cur().params.extend(m.clone());
    }
    match ui.get("zoom") {
        Some(Value::String(s)) if s == "fit" => d.zoom = 0.0,
        Some(v) if v.is_number() => d.zoom = (v.as_f64().unwrap_or(1.0) as f32).clamp(0.01, 32.0),
        _ => {}
    }
    Ok(d.describe())
}

/// Recomputes the preview texture when the stack or the view changed.
fn update_preview(d: &mut GalleryDialog, ctx: &egui::Context, area: ERect) {
    let (cw, ch) = (d.canvas.width() as f32, d.canvas.height() as f32);
    let zoom = if d.zoom > 0.0 { d.zoom } else { ((area.width() - 24.0) / cw).min((area.height() - 24.0) / ch).clamp(0.005, 1.0) };
    if d.zoom <= 0.0 {
        d.center = [d.canvas.x0 as f32 + cw / 2.0, d.canvas.y0 as f32 + ch / 2.0];
    }
    let (hw, hh) = (area.width() / zoom / 2.0, area.height() / zoom / 2.0);
    let vis =
        Rect::new((d.center[0] - hw).floor() as i32, (d.center[1] - hh).floor() as i32, (d.center[0] + hw).ceil() as i32, (d.center[1] + hh).ceil() as i32)
            .intersect(&d.canvas);
    let k = (1.0 / zoom).ceil().max(1.0) as u32;
    let params = d.params();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    params.to_string().hash(&mut h);
    (vis.x0, vis.y0, vis.x1, vis.y1, k).hash(&mut h);
    let key = h.finish();
    let screen = ERect::from_min_max(
        area.center() + vec2((vis.x0 as f32 - d.center[0]) * zoom, (vis.y0 as f32 - d.center[1]) * zoom),
        area.center() + vec2((vis.x1 as f32 - d.center[0]) * zoom, (vis.y1 as f32 - d.center[1]) * zoom),
    );
    if d.shown.is_some_and(|(k0, _)| k0 == key) {
        d.shown = Some((key, screen));
        return;
    }
    if vis.is_empty() {
        return;
    }
    let t0 = crate::gpu_canvas::now_ms();
    let small = sample(&d.src, vis, k);
    let out = render(&small, &params);
    let (w, hgt) = (vis.width().div_ceil(k) as usize, vis.height().div_ceil(k) as usize);
    let img = image(&out, w, hgt);
    match &mut d.tex {
        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
        None => d.tex = Some(ctx.load_texture("gallery-preview", img, egui::TextureOptions::LINEAR)),
    }
    d.preview_ms = crate::gpu_canvas::now_ms() - t0;
    d.shown = Some((key, screen));
}

/// Renders up to `budget` missing thumbnails (spread over frames so opening stays instant).
fn update_thumbs(d: &mut GalleryDialog, ctx: &egui::Context, budget: usize) {
    let mut done = 0;
    for (i, f) in GalleryFilter::ALL.iter().enumerate() {
        if d.thumbs[i].is_some() || !d.open[GALLERY_CATEGORIES.iter().position(|c| *c == f.category()).unwrap_or(0)] {
            continue;
        }
        if done == budget {
            ctx.request_repaint();
            return;
        }
        let p = json!({"effects": [{"filter": f.key()}], "foreground": d.foreground, "background": d.background});
        let out = render(&d.thumb_src, &p);
        let b = d.thumb_src.content_bounds();
        let img = image(&out, b.x1.max(1) as usize, b.y1.max(1) as usize);
        d.thumbs[i] = Some(ctx.load_texture(format!("gallery-thumb-{}", f.key()), img, egui::TextureOptions::LINEAR));
        done += 1;
    }
}

/// Draws the dialog (a full-window layer over the app).
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut action: Option<&str> = None;
    egui::Area::new(egui::Id::new("gallery-dialog")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let Some(d) = app.distort.gallery.as_mut() else { return };
        let (full, _) = ui.allocate_exact_size(screen.size(), Sense::hover());
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0.0, t.chrome);
        let title = ERect::from_min_size(full.min, vec2(full.width(), 30.0));
        painter.rect_filled(title, 0.0, t.dock);
        painter.line_segment([title.left_bottom(), title.right_bottom()], Stroke::new(1.0, t.separator));
        let pct = if d.zoom > 0.0 { format!("{:.0}%", d.zoom * 100.0) } else { tl!("Fit").into() };
        let name = d.effects.get(d.selected).map_or("", |e| e.filter.name());
        painter.text(title.center(), Align2::CENTER_CENTER, format!("{name} ({}, {pct})", d.layer_name), FontId::proportional(13.0), t.text);
        let body = ERect::from_min_max(pos2(full.left(), title.bottom()), full.max);
        let right = ERect::from_min_size(pos2(body.right() - RIGHT_W, body.top()), vec2(RIGHT_W, body.height()));
        let mid = ERect::from_min_size(pos2(right.left() - MID_W, body.top()), vec2(MID_W, body.height()));
        let area = ERect::from_min_max(body.min, pos2(mid.left(), body.bottom() - 30.0));
        let zoom_bar = ERect::from_min_max(pos2(body.left(), area.bottom()), pos2(mid.left(), body.bottom()));

        // ---- Preview ----
        painter.rect_filled(area, 0.0, t.canvas);
        update_preview(d, ctx, area);
        let clip = painter.with_clip_rect(area);
        if let (Some(tex), Some((_, r))) = (&d.tex, d.shown) {
            widgets::checker(&clip, r.intersect(area), 8.0);
            clip.image(tex.id(), r, ERect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            clip.rect_stroke(r, 0.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Outside);
        }
        let resp = ui.interact(area, egui::Id::new("gallery-preview"), Sense::drag());
        if resp.dragged() && d.zoom > 0.0 {
            let dl = resp.drag_delta() / d.zoom;
            d.center = [d.center[0] - dl.x, d.center[1] - dl.y];
        }
        // ---- Zoom bar ----
        painter.rect_filled(zoom_bar, 0.0, t.dock);
        let mut zb = ui.new_child(egui::UiBuilder::new().max_rect(zoom_bar.shrink2(vec2(10.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let steps = [0.0f32, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0];
        if crate::icons::button(&mut zb, "minus", 22.0, false, tl!("Zoom out")).clicked() {
            d.zoom = steps.iter().rev().copied().find(|&s| s > 0.0 && s < d.zoom.max(0.0) - 1e-3).unwrap_or(0.0);
        }
        if crate::icons::button(&mut zb, "plus", 22.0, false, tl!("Zoom in")).clicked() {
            d.zoom = steps.iter().copied().find(|&s| s > d.zoom + 1e-3).unwrap_or(4.0);
        }
        let mut z = d.zoom;
        let opts: Vec<(f32, &str)> =
            vec![(0.0, tl!("Fit in View")), (0.125, "12.5%"), (0.25, "25%"), (0.5, "50%"), (1.0, "100%"), (2.0, "200%"), (4.0, "400%")];
        if widgets::dropdown(&mut zb, "gallery-zoom", &mut z, &opts, 110.0) {
            d.zoom = z;
        }
        zb.label(egui::RichText::new(format!("preview {:.0} ms", d.preview_ms)).color(t.text_faint).size(11.0));

        // ---- Category folders with thumbnails ----
        painter.rect_filled(mid, 0.0, t.dock);
        painter.line_segment([mid.left_top(), mid.left_bottom()], Stroke::new(1.0, t.separator));
        update_thumbs(d, ctx, 6);
        let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(mid.shrink2(vec2(8.0, 8.0))));
        egui::ScrollArea::vertical().id_salt("gallery-tree").show(&mut mu, |ui| {
            for (ci, cat) in GALLERY_CATEGORIES.iter().enumerate() {
                let icon = if d.open[ci] { "folder-open" } else { "folder" };
                let r = ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    crate::icons::paint(ui, ir, icon, 14.0, t.icon);
                    ui.add(egui::Label::new(egui::RichText::new(*cat).color(t.text)).sense(Sense::click()))
                });
                if r.inner.clicked() {
                    d.open[ci] = !d.open[ci];
                }
                if !d.open[ci] {
                    continue;
                }
                let members: Vec<(usize, GalleryFilter)> = GalleryFilter::ALL.iter().copied().enumerate().filter(|(_, f)| f.category() == *cat).collect();
                egui::Grid::new(format!("gallery-cat-{ci}")).spacing([6.0, 6.0]).show(ui, |ui| {
                    for (n, (i, f)) in members.iter().enumerate() {
                        let selected = d.effects.get(d.selected).is_some_and(|e| e.filter == *f);
                        let cell = ui.vertical(|ui| {
                            let (r, resp) = ui.allocate_exact_size(vec2(THUMB[0] as f32, THUMB[1] as f32), Sense::click());
                            match &d.thumbs[*i] {
                                Some(tex) => {
                                    ui.painter().image(tex.id(), r, ERect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                }
                                None => {
                                    ui.painter().rect_filled(r, 2.0, t.field);
                                }
                            }
                            let stroke = if selected { Stroke::new(2.0, t.accent) } else { Stroke::new(1.0, t.separator) };
                            ui.painter().rect_stroke(r, 0.0, stroke, egui::StrokeKind::Outside);
                            ui.add_sized(
                                [THUMB[0] as f32, 14.0],
                                egui::Label::new(egui::RichText::new(f.name()).size(10.5).color(if selected { t.text } else { t.text_dim })).truncate(),
                            );
                            resp
                        });
                        if cell.inner.clicked() {
                            d.set_filter(*f);
                        }
                        if n % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
                ui.add_space(4.0);
            }
        });

        // ---- Right: buttons, filter settings, effect layers ----
        painter.rect_filled(right, 0.0, t.dock);
        painter.line_segment([right.left_top(), right.left_bottom()], Stroke::new(1.0, t.separator));
        let inner = right.shrink2(vec2(12.0, 10.0));
        let mut ru = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        ru.spacing_mut().item_spacing.y = 6.0;
        ru.horizontal(|ui| {
            if widgets::primary_button(ui, tl!("OK"), 120.0).clicked() {
                action = Some("ok");
            }
            if widgets::secondary_button(ui, tl!("Cancel"), 120.0).clicked() {
                action = Some("cancel");
            }
        });
        ru.add_space(4.0);
        let mut cur = d.effects[d.selected].filter;
        let names: Vec<(GalleryFilter, &str)> = GalleryFilter::ALL.iter().map(|f| (*f, f.name())).collect();
        if widgets::dropdown(&mut ru, "gallery-filter", &mut cur, &names, inner.width() - 8.0) {
            d.set_filter(cur);
        }
        widgets::hairline(&mut ru);
        let list_h = 190.0;
        let params_h = (inner.height() - 110.0 - list_h).max(80.0);
        egui::ScrollArea::vertical().id_salt("gallery-params").max_height(params_h).show(&mut ru, |ui| {
            let sel = d.selected;
            let e = &mut d.effects[sel];
            for p in parse_spec(e.filter.params_doc()) {
                let label = crate::filter_dialog::label(&p.key);
                match p.kind {
                    Kind::Range { min, max, default } => {
                        let mut v = e.params.get(&p.key).and_then(Value::as_f64).unwrap_or(default as f64) as f32;
                        widgets::slider_row(ui, &label, &mut v, min..=max, "", None);
                        e.params.insert(p.key, json!(v.round()));
                    }
                    Kind::Choice(options) => {
                        let mut c = e.params.get(&p.key).and_then(Value::as_str).unwrap_or(&options[0]).to_string();
                        let labels: Vec<String> = options.iter().map(|o| crate::filter_dialog::label(o)).collect();
                        let opts: Vec<(String, &str)> = options.iter().cloned().zip(labels.iter().map(String::as_str)).collect();
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim));
                            widgets::dropdown(ui, &format!("gallery-{}", p.key), &mut c, &opts, 140.0);
                        });
                        e.params.insert(p.key, json!(c));
                    }
                    Kind::Bool(b0) => {
                        let mut b = e.params.get(&p.key).and_then(Value::as_bool).unwrap_or(b0);
                        widgets::checkbox(ui, &mut b, &label);
                        e.params.insert(p.key, json!(b));
                    }
                    Kind::Json if p.key == "glowColor" => {
                        let c = e
                            .params
                            .get("glowColor")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(Value::as_f64).map(|v| v as f32).collect::<Vec<_>>())
                            .filter(|v| v.len() >= 3);
                        let neon = photocraft_algo::GalleryEffect::new(GalleryFilter::NeonGlow).color;
                        let mut rgb = c.map_or([neon[0], neon[1], neon[2]], |v| [v[0], v[1], v[2]]);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(tl!("Glow Color")).color(t.text_dim));
                            ui.color_edit_button_rgb(&mut rgb);
                        });
                        e.params.insert("glowColor".into(), json!([rgb[0], rgb[1], rgb[2], 1.0]));
                    }
                    _ => {}
                }
            }
            if e.filter.uses_colours() {
                ui.label(egui::RichText::new(tl!("Uses the foreground and background colours.")).color(t.text_faint).size(11.0));
            }
        });
        // Effect layers (top of the list = applied last).
        let list_top = inner.bottom() - list_h;
        let lr = ERect::from_min_max(pos2(inner.left(), list_top), inner.max);
        let mut lu = ui.new_child(egui::UiBuilder::new().max_rect(lr));
        widgets::hairline(&mut lu);
        egui::ScrollArea::vertical().id_salt("gallery-layers").max_height(list_h - 44.0).show(&mut lu, |ui| {
            let mut toggle = None;
            for i in (0..d.effects.len()).rev() {
                let sel = i == d.selected;
                let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                if sel {
                    ui.painter().rect_filled(row, 2.0, t.accent_soft);
                }
                let eye = ERect::from_min_size(row.min + vec2(4.0, 4.0), vec2(16.0, 16.0));
                crate::icons::paint(ui, eye, if d.effects[i].visible { "eye" } else { "eye-off" }, 14.0, t.icon);
                ui.painter().text(
                    row.left_center() + vec2(28.0, 0.0),
                    Align2::LEFT_CENTER,
                    d.effects[i].filter.name(),
                    FontId::proportional(12.5),
                    if d.effects[i].visible { t.text } else { t.text_faint },
                );
                if resp.clicked() {
                    if resp.interact_pointer_pos().is_some_and(|p| p.x < eye.right() + 4.0) {
                        toggle = Some(i);
                    } else {
                        d.selected = i;
                    }
                }
            }
            if let Some(i) = toggle {
                d.effects[i].visible = !d.effects[i].visible;
            }
        });
        lu.horizontal(|ui| {
            if crate::icons::button(ui, "chevron-up", 24.0, false, tl!("Move effect layer up")).clicked() {
                let s = d.selected;
                d.move_to(s, (s + 1).min(d.effects.len() - 1));
            }
            if crate::icons::button(ui, "chevron-down", 24.0, false, tl!("Move effect layer down")).clicked() {
                let s = d.selected;
                d.move_to(s, s.saturating_sub(1));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::button(ui, "trash", 24.0, false, tl!("Delete effect layer")).clicked() {
                    d.delete();
                }
                if crate::icons::button(ui, "file-plus", 24.0, false, tl!("New effect layer")).clicked() {
                    d.add();
                }
            });
        });
    });
    match action {
        Some("ok") => {
            let _ = commit(app);
        }
        Some("cancel") => app.distort.gallery = None,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", json!({"width": 90, "height": 60, "depth": 16})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.session
            .edit("paint", |doc, active| {
                let s = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
                for y in 0..60 {
                    for x in 0..90 {
                        let v = ((x / 6 + y / 6) % 2) as f32;
                        s.write_pixel(x, y, &[v, 0.4, 1.0 - v, 1.0]);
                    }
                }
                Ok(())
            })
            .unwrap();
        app.sync_views();
        app
    }

    #[test]
    fn stack_editing_and_commit_match_the_engine() {
        let ctx = egui::Context::default();
        let mut app = app();
        let r = crate::distort_ui::menu(&mut app, &ctx, "filter.filterGallery", &json!({})).unwrap().unwrap();
        assert_eq!(r["gallery"]["effects"].as_array().unwrap().len(), 1);
        control(&mut app, &json!({"filter": "cutout", "params": {"numberOfLevels": 3}})).unwrap();
        control(&mut app, &json!({"add": true})).unwrap();
        control(&mut app, &json!({"filter": "texturizer", "params": {"texture": "brick", "relief": 20}})).unwrap();
        control(&mut app, &json!({"add": true})).unwrap();
        control(&mut app, &json!({"filter": "glowingEdges"})).unwrap();
        control(&mut app, &json!({"toggle": 2})).unwrap();
        let d = control(&mut app, &json!({"move": {"from": 1, "to": 0}})).unwrap();
        let keys: Vec<&str> = d["effects"].as_array().unwrap().iter().map(|e| e["filter"].as_str().unwrap()).collect();
        assert_eq!(keys, ["texturizer", "cutout", "glowingEdges"]);
        assert_eq!(d["effects"][2]["visible"], json!(false));
        // Preview pixels = engine result on the same pixels.
        let params = app.distort.gallery.as_ref().unwrap().params();
        let before = app.session.active().unwrap().history.past_len();
        control(&mut app, &json!({"commit": true})).unwrap();
        assert!(app.distort.gallery.is_none());
        assert_eq!(app.session.active().unwrap().history.past_len(), before + 1);
        let (id, p) = app.session.journal.last().cloned().unwrap();
        assert_eq!(id, "filter.filterGallery");
        assert_eq!(p["effects"], params["effects"]);
        // Re-opening starts from the last stack.
        crate::distort_ui::menu(&mut app, &ctx, "filter.filterGallery", &json!({})).unwrap().unwrap();
        assert_eq!(app.distort.gallery.as_ref().unwrap().effects.len(), 3);
        control(&mut app, &json!({"delete": true})).unwrap();
        assert_eq!(app.distort.gallery.as_ref().unwrap().effects.len(), 2);
        control(&mut app, &json!({"cancel": true})).unwrap();
        assert_eq!(app.session.active().unwrap().history.past_len(), before + 1, "cancel records nothing");
    }

    #[test]
    fn preview_and_thumbnails_render() {
        let ctx = egui::Context::default();
        let mut app = app();
        open(&mut app).unwrap();
        let d = app.distort.gallery.as_mut().unwrap();
        update_preview(d, &ctx, ERect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0)));
        assert!(d.tex.is_some() && d.shown.is_some());
        d.open = [true; 6];
        update_thumbs(d, &ctx, 100);
        assert!(d.thumbs.iter().all(Option::is_some));
        // Preview equals the engine's algorithm on the sampled pixels.
        let src = sample(&d.src, d.canvas, 1);
        let a = render(&src, &d.params());
        let mut s2 = photocraft_engine::Session::new();
        s2.add_document(app.session.active().unwrap().doc.as_ref().clone(), None);
        s2.select_layer(d.layer).unwrap();
        s2.execute("filter.filterGallery", d.params()).unwrap();
        let st = s2.active().unwrap();
        let b = st.doc.layer(d.layer).unwrap().surface().unwrap().read_region(d.canvas);
        let worst = a.read_region(d.canvas).iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
        assert!(worst < 2e-3, "preview differs from the result by {worst}");
    }
}
