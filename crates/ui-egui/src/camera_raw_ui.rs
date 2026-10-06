//! Filter › Camera Raw Filter…: a large dialog like Adobe Camera Raw's (preview on the left,
//! edit panels on the right: Basic, Curve, Detail, Color Mixer, Color Grading, Effects).
//!
//! The preview runs [`photocraft_algo::camera_raw::develop`] on a CPU proxy of the layer
//! (≤ 900 px, `pixel_scale` keeps pixel radii true to the full image), recomputed when a
//! control changes. OK runs `filter.cameraRaw` with the non-default settings, so the result is
//! one history step (or a smart filter on a smart object) and exactly replayable.
//!
//! Control channel: `ui.menu.invoke {"id":"filter.cameraRaw","params":{"ui":{"set":{…},
//! "commit":true | "cancel":true}}}`; the reply describes the dialog.

use egui::{Align2, Color32, FontId, Pos2, Rect as ERect, Sense, Stroke, TextureHandle, pos2, vec2};
use photocraft_algo::camera_raw::{CameraRaw, Wheel, curve_lut};
use photocraft_doc::LayerId;
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;
use crate::widgets;

const PROXY_SIDE: usize = 900;
const PANEL_W: f32 = 330.0;
const BANDS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

pub struct CameraRawDialog {
    pub layer: LayerId,
    layer_name: String,
    pub params: CameraRaw,
    /// Proxy pixels (straight RGBA) and size.
    proxy: Vec<[f32; 4]>,
    pw: usize,
    ph: usize,
    full_w: usize,
    float: bool,
    tex: Option<TextureHandle>,
    before_tex: Option<TextureHandle>,
    dirty: bool,
    pub show_before: bool,
    mixer_tab: usize,
    pub render_ms: f64,
}

impl CameraRawDialog {
    pub fn describe(&self) -> Value {
        json!({"layer": self.layer.0, "params": serde_json::to_value(&self.params).unwrap_or(Value::Null), "proxy": [self.pw, self.ph], "renderMs": self.render_ms, "before": self.show_before})
    }

    /// Params for the engine: only the settings that differ from the defaults.
    pub fn command_params(&self) -> Value {
        let full = serde_json::to_value(&self.params).unwrap_or(json!({}));
        let def = serde_json::to_value(CameraRaw::default()).unwrap_or(json!({}));
        let mut out = serde_json::Map::new();
        if let (Value::Object(f), Value::Object(d)) = (full, def) {
            for (k, v) in f {
                if k != "pixelScale" && d.get(&k) != Some(&v) {
                    out.insert(k, v);
                }
            }
        }
        Value::Object(out)
    }

    fn image(px: &[[f32; 4]], w: usize, h: usize) -> egui::ColorImage {
        let enc = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        egui::ColorImage::new([w, h], px.iter().map(|q| Color32::from_rgba_unmultiplied(enc(q[0]), enc(q[1]), enc(q[2]), enc(q[3]))).collect())
    }

    fn render(&mut self, ctx: &egui::Context) {
        let t0 = crate::gpu_canvas::now_ms();
        let mut px = self.proxy.clone();
        let mut p = self.params.clone();
        p.pixel_scale = (self.pw as f32 / self.full_w.max(1) as f32).min(1.0);
        photocraft_algo::camera_raw::develop(&mut px, self.pw, self.ph, &p, self.float);
        let img = Self::image(&px, self.pw, self.ph);
        match &mut self.tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("camera-raw-after", img, egui::TextureOptions::LINEAR)),
        }
        if self.before_tex.is_none() {
            self.before_tex = Some(ctx.load_texture("camera-raw-before", Self::image(&self.proxy, self.pw, self.ph), egui::TextureOptions::LINEAR));
        }
        self.render_ms = crate::gpu_canvas::now_ms() - t0;
        self.dirty = false;
    }
}

/// Opens the dialog on the active layer.
pub fn open(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<(), String> {
    photocraft_engine::commands::find("filter.cameraRaw").map(|c| (c.enabled)(&app.session)).unwrap_or(Err("unknown command".into()))?;
    let (layer, surf, _) = crate::distort_ui::active_pixels(app)?;
    let st = app.session.active().ok_or("no document")?;
    let canvas = st.doc.bounds();
    let name = st.doc.layer(layer).map(|l| l.name.clone()).unwrap_or_default();
    let area = surf.content_bounds().intersect(&canvas);
    let area = if area.is_empty() { canvas } else { area };
    let (w, h) = (area.width() as usize, area.height() as usize);
    let k = w.max(h).div_ceil(PROXY_SIDE).max(1);
    let (pw, ph) = (w.div_ceil(k), h.div_ceil(k));
    let mut proxy = vec![[0.0f32; 4]; pw * ph];
    let mut row = vec![[0.0f32; 4]; w];
    for py in 0..ph {
        // Box-average k×k blocks.
        let mut acc = vec![[0.0f32; 5]; pw];
        for dy in 0..k {
            let y = area.y0 + (py * k + dy).min(h - 1) as i32;
            surf.read_rgba_into(Rect::new(area.x0, y, area.x1, y + 1), &mut row);
            for (x, q) in row.iter().enumerate() {
                let a = &mut acc[x / k];
                for c in 0..4 {
                    a[c] += q[c];
                }
                a[4] += 1.0;
            }
        }
        for (x, a) in acc.iter().enumerate() {
            proxy[py * pw + x] = [a[0] / a[4], a[1] / a[4], a[2] / a[4], a[3] / a[4]];
        }
    }
    let float = surf.format().sample == photocraft_color::SampleType::F32;
    let mut d = CameraRawDialog {
        layer,
        layer_name: name,
        params: CameraRaw::default(),
        proxy,
        pw,
        ph,
        full_w: w,
        float,
        tex: None,
        before_tex: None,
        dirty: true,
        show_before: false,
        mixer_tab: 1,
        render_ms: 0.0,
    };
    d.render(ctx);
    app.camera_raw = Some(d);
    Ok(())
}

/// Menu / control-channel entry point. `None` when the call isn't for this dialog.
pub fn menu(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "filter.cameraRaw" {
        return None;
    }
    let empty = params.as_object().is_none_or(|o| o.is_empty());
    if empty {
        return Some(open(app, ctx).map(|_| app.camera_raw.as_ref().map(|d| d.describe()).unwrap_or(Value::Null)));
    }
    let ui = params.get("ui")?;
    if app.camera_raw.is_none()
        && let Err(e) = open(app, ctx)
    {
        return Some(Err(e));
    }
    if let Some(set) = ui.get("set") {
        let d = app.camera_raw.as_mut()?;
        let mut cur = serde_json::to_value(&d.params).unwrap_or(json!({}));
        if let (Value::Object(c), Value::Object(s)) = (&mut cur, set) {
            for (k, v) in s {
                c.insert(k.clone(), v.clone());
            }
        }
        match serde_json::from_value::<CameraRaw>(cur) {
            Ok(p) => {
                d.params = p;
                d.render(ctx);
            }
            Err(e) => return Some(Err(format!("bad Camera Raw settings: {e}"))),
        }
    }
    if let Some(b) = ui.get("before").and_then(Value::as_bool)
        && let Some(d) = app.camera_raw.as_mut()
    {
        d.show_before = b;
    }
    if ui.get("cancel").and_then(Value::as_bool) == Some(true) {
        app.camera_raw = None;
        return Some(Ok(json!({"cancelled": true})));
    }
    if ui.get("commit").and_then(Value::as_bool) == Some(true) {
        return Some(commit(app));
    }
    Some(Ok(app.camera_raw.as_ref().map(|d| d.describe()).unwrap_or(Value::Null)))
}

fn commit(app: &mut PhotocraftApp) -> Result<Value, String> {
    let d = app.camera_raw.take().ok_or(tl!("Camera Raw isn't open"))?;
    let mut p = d.command_params();
    p["layer"] = json!(d.layer.0);
    app.run("filter.cameraRaw", p)
}

fn temp_stops() -> Vec<Color32> {
    vec![Color32::from_rgb(70, 120, 230), Color32::from_rgb(200, 200, 200), Color32::from_rgb(235, 200, 60)]
}
fn tint_stops() -> Vec<Color32> {
    vec![Color32::from_rgb(70, 190, 80), Color32::from_rgb(200, 200, 200), Color32::from_rgb(210, 80, 200)]
}

/// One labelled slider; marks the dialog dirty when it moves.
fn row(ui: &mut egui::Ui, dirty: &mut bool, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, grad: Option<&[Color32]>) {
    let r = widgets::slider_row(ui, label, v, range, "", grad);
    if r.changed() {
        *dirty = true;
    }
    if r.double_clicked() {
        *v = 0.0;
        *dirty = true;
    }
}

fn section(ui: &mut egui::Ui, title: &str, open: bool, body: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new(tl!(&title)).font(FontId::proportional(13.0))).default_open(open).show(ui, body);
}

fn wheel(ui: &mut egui::Ui, dirty: &mut bool, title: &str, w: &mut Wheel) {
    widgets::section_label(ui, title);
    let hs = widgets::hue_stops();
    row(ui, dirty, tl!("Hue"), &mut w.hue, 0.0..=360.0, Some(&hs));
    row(ui, dirty, tl!("Saturation"), &mut w.sat, 0.0..=100.0, None);
    row(ui, dirty, tl!("Luminance"), &mut w.lum, -100.0..=100.0, None);
}

/// Small point-curve editor (master channel): click to add, drag to move, right-click to delete.
fn curve_editor(ui: &mut egui::Ui, p: &mut CameraRaw, dirty: &mut bool) {
    let t = Tokens::get(ui.ctx());
    let side = ui.available_width().min(260.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, t.field);
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment([pos2(rect.left() + f * side, rect.top()), pos2(rect.left() + f * side, rect.bottom())], Stroke::new(1.0, t.separator));
        painter.line_segment([pos2(rect.left(), rect.top() + f * side), pos2(rect.right(), rect.top() + f * side)], Stroke::new(1.0, t.separator));
    }
    let to_screen = |x: f32, y: f32| pos2(rect.left() + x / 255.0 * side, rect.bottom() - y / 255.0 * side);
    let from_screen = |q: Pos2| [((q.x - rect.left()) / side * 255.0).clamp(0.0, 255.0), ((rect.bottom() - q.y) / side * 255.0).clamp(0.0, 255.0)];
    if p.point_curve.len() < 2 {
        p.point_curve = vec![[0.0, 0.0], [255.0, 255.0]];
    }
    // Interaction.
    let drag_id = ui.id().with("cr-curve-drag");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    if let Some(pos) = resp.interact_pointer_pos() {
        let q = from_screen(pos);
        if resp.drag_started() || resp.clicked() {
            let near = p.point_curve.iter().position(|c| (to_screen(c[0], c[1]) - pos).length() < 8.0);
            if resp.secondary_clicked() {
                if let Some(i) = near.filter(|i| *i != 0 && *i + 1 != p.point_curve.len()) {
                    p.point_curve.remove(i);
                    *dirty = true;
                }
            } else {
                dragging = Some(near.unwrap_or_else(|| {
                    p.point_curve.push(q);
                    p.point_curve.sort_by(|a, b| a[0].total_cmp(&b[0]));
                    *dirty = true;
                    p.point_curve.iter().position(|c| *c == q).unwrap_or(0)
                }));
            }
        }
        if resp.dragged()
            && let Some(i) = dragging
        {
            let n = p.point_curve.len();
            let lo = if i == 0 { 0.0 } else { p.point_curve[i - 1][0] + 1.0 };
            let hi = if i + 1 == n { 255.0 } else { p.point_curve[i + 1][0] - 1.0 };
            p.point_curve[i] = [q[0].clamp(lo, hi.max(lo)), q[1]];
            *dirty = true;
        }
    }
    if resp.drag_stopped() {
        dragging = None;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));
    // Curve (point curve after the parametric one).
    let lut = curve_lut(&p.point_curve, 256);
    let pts: Vec<Pos2> = (0..256).map(|i| to_screen(i as f32, lut[i] * 255.0)).collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, t.text)));
    for c in &p.point_curve {
        painter.circle_stroke(to_screen(c[0], c[1]), 4.0, Stroke::new(1.5, t.accent));
    }
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.camera_raw.is_none() {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut action: Option<&str> = None;
    egui::Area::new(egui::Id::new("camera-raw-dialog")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let Some(d) = app.camera_raw.as_mut() else { return };
        if d.dirty {
            d.render(ctx);
        }
        let (full, _) = ui.allocate_exact_size(screen.size(), Sense::click());
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0.0, t.chrome);
        let title = ERect::from_min_size(full.min, vec2(full.width(), 30.0));
        painter.rect_filled(title, 0.0, t.dock);
        painter.line_segment([title.left_bottom(), title.right_bottom()], Stroke::new(1.0, t.separator));
        painter.text(title.center(), Align2::CENTER_CENTER, format!("Camera Raw Filter ({})", d.layer_name), FontId::proportional(13.0), t.text);
        let footer_h = 48.0;
        let body = ERect::from_min_max(pos2(full.left(), title.bottom()), pos2(full.right(), full.bottom() - footer_h));
        // Preview.
        let view = ERect::from_min_max(body.min, pos2(body.right() - PANEL_W, body.bottom())).shrink(16.0);
        painter.rect_filled(view, 0.0, t.canvas);
        let tex = if d.show_before { d.before_tex.as_ref() } else { d.tex.as_ref() };
        if let Some(tex) = tex {
            let s = (view.width() / d.pw as f32).min(view.height() / d.ph as f32);
            let r = ERect::from_center_size(view.center(), vec2(d.pw as f32 * s, d.ph as f32 * s));
            widgets::checker(&painter, r, 8.0);
            painter.image(tex.id(), r, ERect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // Panels.
        let right = ERect::from_min_max(pos2(body.right() - PANEL_W, body.top()), body.max);
        painter.rect_filled(right, 0.0, t.dock);
        painter.line_segment([right.left_top(), right.left_bottom()], Stroke::new(1.0, t.separator));
        let mut props = ui.new_child(egui::UiBuilder::new().max_rect(right.shrink2(vec2(14.0, 10.0))));
        let mut dirty = false;
        egui::ScrollArea::vertical().id_salt("camera-raw-props").show(&mut props, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let p = &mut d.params;
            let (ts, tn) = (temp_stops(), tint_stops());
            section(ui, tl!("Basic"), true, |ui| {
                widgets::section_label(ui, tl!("White Balance: As Shot"));
                row(ui, &mut dirty, tl!("Temperature"), &mut p.temperature, -100.0..=100.0, Some(&ts));
                row(ui, &mut dirty, tl!("Tint"), &mut p.tint, -100.0..=100.0, Some(&tn));
                row(ui, &mut dirty, tl!("Exposure"), &mut p.exposure, -5.0..=5.0, None);
                row(ui, &mut dirty, tl!("Contrast"), &mut p.contrast, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Highlights"), &mut p.highlights, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Shadows"), &mut p.shadows, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Whites"), &mut p.whites, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Blacks"), &mut p.blacks, -100.0..=100.0, None);
                widgets::hairline(ui);
                row(ui, &mut dirty, tl!("Texture"), &mut p.texture, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Clarity"), &mut p.clarity, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Dehaze"), &mut p.dehaze, -100.0..=100.0, None);
                widgets::hairline(ui);
                row(ui, &mut dirty, tl!("Vibrance"), &mut p.vibrance, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Saturation"), &mut p.saturation, -100.0..=100.0, None);
            });
            section(ui, tl!("Curve"), false, |ui| {
                curve_editor(ui, p, &mut dirty);
                row(ui, &mut dirty, tl!("Highlights"), &mut p.curve_highlights, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Lights"), &mut p.curve_lights, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Darks"), &mut p.curve_darks, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Shadows"), &mut p.curve_shadows, -100.0..=100.0, None);
            });
            section(ui, tl!("Detail"), false, |ui| {
                widgets::section_label(ui, tl!("Sharpening"));
                row(ui, &mut dirty, tl!("Amount"), &mut p.sharpen_amount, 0.0..=150.0, None);
                row(ui, &mut dirty, tl!("Radius"), &mut p.sharpen_radius, 0.5..=3.0, None);
                row(ui, &mut dirty, tl!("Detail"), &mut p.sharpen_detail, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Masking"), &mut p.sharpen_masking, 0.0..=100.0, None);
                widgets::section_label(ui, tl!("Noise Reduction"));
                row(ui, &mut dirty, tl!("Luminance"), &mut p.noise_luminance, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Luminance Detail"), &mut p.noise_luminance_detail, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Color"), &mut p.noise_color, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Color Detail"), &mut p.noise_color_detail, 0.0..=100.0, None);
            });
            section(ui, tl!("Color Mixer"), false, |ui| {
                ui.horizontal(|ui| {
                    for (i, name) in [tl!("Hue"), tl!("Saturation"), tl!("Luminance")].iter().enumerate() {
                        if widgets::pill_tab(ui, name, d.mixer_tab == i).clicked() {
                            d.mixer_tab = i;
                        }
                    }
                });
                let arr = match d.mixer_tab {
                    0 => &mut p.hsl_hue,
                    1 => &mut p.hsl_sat,
                    _ => &mut p.hsl_lum,
                };
                for (k, name) in BANDS.iter().enumerate() {
                    let c = photocraft_algo::camera_raw::HSL_BANDS[k];
                    let base = hue_color(c);
                    let grad = [Color32::from_gray(128), base];
                    row(ui, &mut dirty, name, &mut arr[k], -100.0..=100.0, Some(&grad));
                }
            });
            section(ui, tl!("Color Grading"), false, |ui| {
                wheel(ui, &mut dirty, tl!("Shadows"), &mut p.grade_shadows);
                wheel(ui, &mut dirty, tl!("Midtones"), &mut p.grade_midtones);
                wheel(ui, &mut dirty, tl!("Highlights"), &mut p.grade_highlights);
                wheel(ui, &mut dirty, tl!("Global"), &mut p.grade_global);
                row(ui, &mut dirty, tl!("Blending"), &mut p.grade_blending, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Balance"), &mut p.grade_balance, -100.0..=100.0, None);
            });
            section(ui, tl!("Effects"), false, |ui| {
                widgets::section_label(ui, tl!("Grain"));
                row(ui, &mut dirty, tl!("Amount"), &mut p.grain_amount, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Size"), &mut p.grain_size, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Roughness"), &mut p.grain_roughness, 0.0..=100.0, None);
                widgets::section_label(ui, tl!("Vignetting"));
                let mut style = p.vignette_style.clone();
                if widgets::dropdown(
                    ui,
                    "cr-vig-style",
                    &mut style,
                    &[
                        ("highlightPriority".to_string(), tl!("Highlight Priority")),
                        ("colorPriority".to_string(), tl!("Color Priority")),
                        ("paintOverlay".to_string(), tl!("Paint Overlay")),
                    ],
                    200.0,
                ) {
                    p.vignette_style = style;
                    dirty = true;
                }
                row(ui, &mut dirty, tl!("Amount"), &mut p.vignette_amount, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Midpoint"), &mut p.vignette_midpoint, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Roundness"), &mut p.vignette_roundness, -100.0..=100.0, None);
                row(ui, &mut dirty, tl!("Feather"), &mut p.vignette_feather, 0.0..=100.0, None);
                row(ui, &mut dirty, tl!("Highlights"), &mut p.vignette_highlights, 0.0..=100.0, None);
            });
        });
        d.dirty |= dirty;
        // Footer.
        let foot = ERect::from_min_max(pos2(full.left(), full.bottom() - footer_h), full.max);
        painter.rect_filled(foot, 0.0, t.dock);
        painter.line_segment([foot.left_top(), foot.right_top()], Stroke::new(1.0, t.separator));
        let mut fu = ui.new_child(egui::UiBuilder::new().max_rect(foot.shrink2(vec2(16.0, 9.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
        if widgets::primary_button(&mut fu, tl!("OK"), 90.0).clicked() {
            action = Some("ok");
        }
        if widgets::secondary_button(&mut fu, tl!("Cancel"), 90.0).clicked() {
            action = Some("cancel");
        }
        fu.add_space(12.0);
        widgets::checkbox(&mut fu, &mut d.show_before, tl!("Before (Y)"));
        fu.label(egui::RichText::new(format!("{:.0} ms", d.render_ms)).color(t.text_faint));
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Y))
        && let Some(d) = app.camera_raw.as_mut()
    {
        d.show_before = !d.show_before;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some("cancel");
    }
    match action {
        Some("ok") => {
            if let Err(e) = commit(app) {
                app.ui.status = e;
            }
        }
        Some("cancel") => app.camera_raw = None,
        _ => {}
    }
}

fn hue_color(h: f32) -> Color32 {
    let h6 = (h.rem_euclid(360.0)) / 60.0;
    let x = 1.0 - ((h6 % 2.0) - 1.0).abs();
    let (r, g, b) = match h6 as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    Color32::from_rgb((r * 220.0) as u8, (g * 220.0) as u8, (b * 220.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_drives_the_engine() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("edit.fill", json!({"color": "#808080"})).unwrap();
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({})).unwrap().unwrap();
        assert_eq!(r["proxy"], json!([64, 48]));
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"exposure": 1.0, "vignetteAmount": -40}}})).unwrap().unwrap();
        assert_eq!(r["params"]["exposure"], 1.0);
        assert_eq!(app.camera_raw.as_ref().unwrap().command_params(), json!({"exposure": 1.0, "vignetteAmount": -40.0}));
        let before = app.session.active().unwrap().doc.layer(app.session.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().rgba(32, 24);
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"commit": true}})).unwrap().unwrap();
        assert!(app.camera_raw.is_none());
        let after = app.session.active().unwrap().doc.layer(app.session.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().rgba(32, 24);
        assert!(after[0] > before[0] + 0.1, "{before:?} → {after:?}");
        // Cancel leaves the document alone.
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({})).unwrap().unwrap();
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"cancel": true}})).unwrap().unwrap();
        assert_eq!(r["cancelled"], true);
        assert!(menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"exposure": "x"}}})).unwrap().is_err());
    }
}
