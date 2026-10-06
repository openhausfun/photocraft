//! Photoshop's Color Picker (Foreground / Background Color) dialog: a 2D colour field and a slider
//! for the selected component (H, S, B, R, G or B radio), new/current swatches, and HSB, RGB, Lab,
//! CMYK and hex fields. The colour lives in the dialog fields (`color` as `#rrggbb`, plus the HSB
//! floats so hue survives greys), so `ui.dialog.set` drives it; OK runs `tools.setColors`.

use egui::{Color32, Mesh, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;
use crate::theme::Tokens;
use crate::widgets;

/// Radio components, Photoshop order: H, S, B, R, G, B.
pub const MODES: &[(&str, &str)] = &[("h", "H:"), ("s", "S:"), ("v", "B:"), ("r", "R:"), ("g", "G:"), ("b", "B:")];

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = (h.rem_euclid(360.0)) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

pub fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d <= 0.0 {
        0.0
    } else if mx == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    [h, if mx <= 0.0 { 0.0 } else { d / mx }, mx]
}

pub fn hex(c: [f32; 3]) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
}

pub fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|v| f32::from(v) / 255.0);
    Some([c(0)?, c(2)?, c(4)?])
}

/// The colour at field position (x, y) in 0..1 (y down) for `mode` with the slider at `z` (0..1),
/// given the current HSV/RGB (used for the components the field doesn't vary).
pub fn field_color(mode: &str, z: f32, x: f32, y: f32) -> [f32; 3] {
    match mode {
        "s" => hsv_to_rgb(x * 360.0, z, 1.0 - y),
        "v" => hsv_to_rgb(x * 360.0, 1.0 - y, z),
        "r" => [z, 1.0 - y, x],
        "g" => [1.0 - y, z, x],
        "b" => [x, 1.0 - y, z],
        _ => hsv_to_rgb(z * 360.0, x, 1.0 - y),
    }
}

/// Field position and slider value of a colour in `mode` (inverse of [`field_color`]).
pub fn locate(mode: &str, hsv: [f32; 3], rgb: [f32; 3]) -> (f32, f32, f32) {
    let h = hsv[0] / 360.0;
    match mode {
        "s" => (h, 1.0 - hsv[2], hsv[1]),
        "v" => (h, 1.0 - hsv[1], hsv[2]),
        "r" => (rgb[2], 1.0 - rgb[1], rgb[0]),
        "g" => (rgb[2], 1.0 - rgb[0], rgb[1]),
        "b" => (rgb[0], 1.0 - rgb[1], rgb[2]),
        _ => (hsv[1], 1.0 - hsv[2], h),
    }
}

/// Colour for slider position `z` (0..1) in `mode`, other components from the current colour.
fn slider_color(mode: &str, z: f32, hsv: [f32; 3], rgb: [f32; 3]) -> [f32; 3] {
    match mode {
        "s" => hsv_to_rgb(hsv[0], z, hsv[2]),
        "v" => hsv_to_rgb(hsv[0], hsv[1], z),
        "r" => [z, rgb[1], rgb[2]],
        "g" => [rgb[0], z, rgb[2]],
        "b" => [rgb[0], rgb[1], z],
        _ => hsv_to_rgb(z * 360.0, 1.0, 1.0),
    }
}

fn c32(c: [f32; 3]) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(q(c[0]), q(c[1]), q(c[2]))
}

/// Open the picker for the foreground or background colour.
pub fn open(app: &mut PhotocraftApp, target: &str) -> u64 {
    let c = if target == "background" { app.session.tools.background } else { app.session.tools.foreground };
    let rgb = [c[0], c[1], c[2]];
    let hsv = rgb_to_hsv(rgb);
    let mut f = Map::new();
    f.insert("__colorPicker".into(), json!(target));
    f.insert("__label".into(), json!(if target == "background" { "Color Picker (Background Color)" } else { "Color Picker (Foreground Color)" }));
    f.insert("color".into(), json!(hex(rgb)));
    f.insert("__orig".into(), json!(hex(rgb)));
    f.insert("__hsv".into(), json!(hsv));
    f.insert("__mode".into(), json!("h"));
    f.insert("__webOnly".into(), json!(false));
    app.ui.open_dialog(DialogKind::Command, f)
}

/// Open the picker on `rgb` for something other than the tool colours: OK runs `command` with
/// `params` plus `"color": "#rrggbb"` (e.g. a gradient stop's colour).
pub fn open_for_command(app: &mut PhotocraftApp, label: &str, rgb: [f32; 3], command: &str, params: Value) -> u64 {
    let hsv = rgb_to_hsv(rgb);
    let mut f = Map::new();
    f.insert("__colorPicker".into(), json!("command"));
    f.insert("__label".into(), json!(label));
    f.insert("__command".into(), json!(command));
    f.insert("__params".into(), params);
    f.insert("color".into(), json!(hex(rgb)));
    f.insert("__orig".into(), json!(hex(rgb)));
    f.insert("__hsv".into(), json!(hsv));
    f.insert("__mode".into(), json!("h"));
    f.insert("__webOnly".into(), json!(false));
    app.ui.open_dialog(DialogKind::Command, f)
}

pub fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key("__colorPicker")
}

/// Current colour as (rgb, hsv), keeping the stored hue/saturation when they still match `color`.
pub fn current(f: &Map<String, Value>) -> ([f32; 3], [f32; 3]) {
    let rgb = f.get("color").and_then(Value::as_str).and_then(parse_hex).unwrap_or([0.0; 3]);
    let stored: Option<[f32; 3]> = f.get("__hsv").and_then(|v| serde_json::from_value(v.clone()).ok());
    let hsv = match stored {
        Some(h) if hex(hsv_to_rgb(h[0], h[1], h[2])) == hex(rgb) => h,
        _ => rgb_to_hsv(rgb),
    };
    (rgb, hsv)
}

fn set_rgb(f: &mut Map<String, Value>, rgb: [f32; 3], hsv: Option<[f32; 3]>) {
    let web = f.get("__webOnly").and_then(Value::as_bool).unwrap_or(false);
    let rgb = if web { rgb.map(|v| (v * 5.0).round() / 5.0) } else { rgb };
    f.insert("color".into(), json!(hex(rgb)));
    f.insert("__hsv".into(), json!(hsv.filter(|_| !web).unwrap_or_else(|| rgb_to_hsv(rgb))));
}

fn grid_mesh(rect: Rect, n: usize, color: impl Fn(f32, f32) -> [f32; 3]) -> Mesh {
    let mut m = Mesh::default();
    for j in 0..=n {
        for i in 0..=n {
            let (x, y) = (i as f32 / n as f32, j as f32 / n as f32);
            m.colored_vertex(pos2(rect.left() + x * rect.width(), rect.top() + y * rect.height()), c32(color(x, y)));
        }
    }
    let w = (n + 1) as u32;
    for j in 0..n as u32 {
        for i in 0..n as u32 {
            let a = j * w + i;
            m.add_triangle(a, a + 1, a + w + 1);
            m.add_triangle(a, a + w + 1, a + w);
        }
    }
    m
}

pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let mode = f.get("__mode").and_then(Value::as_str).unwrap_or("h").to_string();
    let (rgb, hsv) = current(f);
    let (fx, fy, fz) = locate(&mode, hsv, rgb);
    ui.horizontal_top(|ui| {
        // Colour field.
        let (field, resp) = ui.allocate_exact_size(vec2(256.0, 256.0), Sense::click_and_drag());
        ui.painter().add(grid_mesh(field, 32, |x, y| field_color(&mode, fz, x, y)));
        ui.painter().rect_stroke(field, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
        let marker = pos2(field.left() + fx * field.width(), field.top() + fy * field.height());
        ui.painter().circle_stroke(marker, 5.0, Stroke::new(1.5, if hsv[2] > 0.6 && hsv[1] < 0.4 { Color32::BLACK } else { Color32::WHITE }));
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            let (x, y) = (((p.x - field.left()) / field.width()).clamp(0.0, 1.0), ((p.y - field.top()) / field.height()).clamp(0.0, 1.0));
            let c = field_color(&mode, fz, x, y);
            let h = match mode.as_str() {
                "h" => Some([fz * 360.0, x, 1.0 - y]),
                "s" => Some([x * 360.0, fz, 1.0 - y]),
                "v" => Some([x * 360.0, 1.0 - y, fz]),
                _ => None,
            };
            set_rgb(f, c, h);
        }
        ui.add_space(6.0);
        // Component slider (hue runs 360° at the top to 0° at the bottom, like Photoshop).
        let (strip, sresp) = ui.allocate_exact_size(vec2(20.0, 256.0), Sense::click_and_drag());
        ui.painter().add(grid_mesh(strip, 32, |_, y| slider_color(&mode, 1.0 - y, hsv, rgb)));
        let sy = strip.top() + (1.0 - fz) * strip.height();
        for (x, dir) in [(strip.left() - 1.0, 1.0f32), (strip.right() + 1.0, -1.0)] {
            let tri = vec![pos2(x, sy), pos2(x - 6.0 * dir, sy - 4.0), pos2(x - 6.0 * dir, sy + 4.0)];
            ui.painter().add(egui::Shape::convex_polygon(tri, t.text, Stroke::NONE));
        }
        if let Some(p) = sresp.interact_pointer_pos().filter(|_| sresp.dragged() || sresp.clicked()) {
            let z = 1.0 - ((p.y - strip.top()) / strip.height()).clamp(0.0, 1.0);
            let h = match mode.as_str() {
                "h" => Some([z * 360.0, hsv[1], hsv[2]]),
                "s" => Some([hsv[0], z, hsv[2]]),
                "v" => Some([hsv[0], hsv[1], z]),
                _ => None,
            };
            let c = h.map_or_else(|| slider_color(&mode, z, hsv, rgb), |h| hsv_to_rgb(h[0], h[1], h[2]));
            set_rgb(f, c, h);
        }
        ui.add_space(14.0);
        ui.vertical(|ui| {
            // new / current swatches.
            ui.label(egui::RichText::new(tl!("new")).size(11.0).color(t.text_dim));
            let (sw, _) = ui.allocate_exact_size(vec2(64.0, 72.0), Sense::hover());
            let orig = f.get("__orig").and_then(Value::as_str).and_then(parse_hex).unwrap_or(rgb);
            ui.painter().rect_filled(Rect::from_min_size(sw.min, vec2(64.0, 36.0)), 0.0, c32(rgb));
            let cur = Rect::from_min_size(sw.min + vec2(0.0, 36.0), vec2(64.0, 36.0));
            ui.painter().rect_filled(cur, 0.0, c32(orig));
            ui.painter().rect_stroke(sw, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
            let click_cur = ui.interact(cur, ui.id().with("cp-current"), Sense::click());
            if click_cur.on_hover_text(tl!("Click to restore the current colour")).clicked() {
                set_rgb(f, orig, None);
            }
            ui.label(egui::RichText::new(tl!("current")).size(11.0).color(t.text_dim));
            ui.add_space(10.0);
            let mut web = f.get("__webOnly").and_then(Value::as_bool).unwrap_or(false);
            if widgets::checkbox(ui, &mut web, tl!("Only Web Colors")).changed() {
                f.insert("__webOnly".into(), json!(web));
                set_rgb(f, rgb, None);
            }
        });
        ui.add_space(10.0);
        ui.vertical(|ui| fields(ui, f, &mode, rgb, hsv));
    });
}

/// Component radios and numeric fields: HSB, RGB, Lab, CMYK and hex.
fn fields(ui: &mut egui::Ui, f: &mut Map<String, Value>, mode: &str, rgb: [f32; 3], hsv: [f32; 3]) {
    let t = Tokens::get(ui.ctx());
    let lab = photocraft_color::convert::srgb_to_lab(rgb);
    let cmyk = photocraft_color::convert::rgb_to_cmyk(rgb);
    let mut edit: Option<([f32; 3], Option<[f32; 3]>)> = None;
    let mut new_mode: Option<&str> = None;
    egui::Grid::new("cp-fields").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
        let vals = [hsv[0], hsv[1] * 100.0, hsv[2] * 100.0, rgb[0] * 255.0, rgb[1] * 255.0, rgb[2] * 255.0];
        let units = ["°", "%", "%", "", "", ""];
        let ranges = [0.0..=360.0, 0.0..=100.0, 0.0..=100.0, 0.0..=255.0, 0.0..=255.0, 0.0..=255.0];
        for i in 0..6 {
            let (key, label) = MODES[i];
            let on = mode == key;
            let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            ui.painter().circle_stroke(r.center(), 5.5, Stroke::new(1.2, if on { t.accent } else { t.text_faint }));
            if on {
                ui.painter().circle_filled(r.center(), 3.0, t.accent);
            }
            if resp.clicked() {
                new_mode = Some(key);
            }
            ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim));
            let mut v = vals[i].round();
            if widgets::value_field(ui, &mut v, ranges[i].clone(), "", 54.0).changed() {
                let (mut h, mut c) = (hsv, rgb);
                if i < 3 {
                    h[i] = if i == 0 { v } else { v / 100.0 };
                    c = hsv_to_rgb(h[0], h[1], h[2]);
                    edit = Some((c, Some(h)));
                } else {
                    c[i - 3] = v / 255.0;
                    edit = Some((c, None));
                }
            }
            ui.label(egui::RichText::new(units[i]).color(t.text_faint));
            ui.end_row();
        }
        for (i, (label, v, range)) in [("L:", lab[0], 0.0..=100.0), ("a:", lab[1], -128.0..=127.0), ("b:", lab[2], -128.0..=127.0)].into_iter().enumerate() {
            ui.label("");
            ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim));
            let mut x = v.round();
            if widgets::value_field(ui, &mut x, range, "", 54.0).changed() {
                let mut l = lab;
                l[i] = x;
                edit = Some((photocraft_color::convert::lab_to_srgb(l).map(|c| c.clamp(0.0, 1.0)), None));
            }
            ui.label("");
            ui.end_row();
        }
        for (i, label) in ["C:", "M:", "Y:", "K:"].into_iter().enumerate() {
            ui.label("");
            ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim));
            let mut x = (cmyk[i] * 100.0).round();
            if widgets::value_field(ui, &mut x, 0.0..=100.0, "", 54.0).changed() {
                let mut k = cmyk;
                k[i] = x / 100.0;
                edit = Some((photocraft_color::convert::cmyk_to_rgb(k).map(|c| c.clamp(0.0, 1.0)), None));
            }
            ui.label(egui::RichText::new("%").color(t.text_faint));
            ui.end_row();
        }
        ui.label("");
        ui.label(egui::RichText::new("#").color(t.text_dim));
        let mut h = hex(rgb).trim_start_matches('#').to_string();
        let r = ui.add(egui::TextEdit::singleline(&mut h).desired_width(54.0).font(crate::theme::mono(12.0)));
        if r.changed()
            && let Some(c) = parse_hex(&h)
        {
            edit = Some((c, None));
        }
        ui.end_row();
    });
    if let Some(m) = new_mode {
        f.insert("__mode".into(), json!(m));
    }
    if let Some((c, h)) = edit {
        set_rgb(f, c, h);
    }
}

/// OK: set the foreground or background colour.
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let target = f.get("__colorPicker").and_then(Value::as_str).unwrap_or("foreground");
    let color = f.get("color").and_then(Value::as_str).unwrap_or("#000000");
    if let Some(cmd) = f.get("__command").and_then(Value::as_str) {
        let mut p = f.get("__params").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
        p["color"] = json!(color);
        return app.run(cmd, p);
    }
    app.run("tools.setColors", json!({ target: color }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips() {
        for c in [[1.0, 0.0, 0.0], [0.2, 0.6, 0.4], [0.5, 0.5, 0.5], [0.0, 0.0, 1.0], [0.9, 0.8, 0.1]] {
            let h = rgb_to_hsv(c);
            let back = hsv_to_rgb(h[0], h[1], h[2]);
            for i in 0..3 {
                assert!((back[i] - c[i]).abs() < 1e-5, "{c:?} -> {h:?} -> {back:?}");
            }
        }
        assert_eq!(hex([1.0, 0.5, 0.0]), "#ff8000");
        assert_eq!(parse_hex("#FF8000").map(hex).as_deref(), Some("#ff8000"));
    }

    #[test]
    fn field_and_locate_are_inverse_in_every_mode() {
        let rgb = [0.8, 0.3, 0.2];
        let hsv = rgb_to_hsv(rgb);
        for (m, _) in MODES {
            let (x, y, z) = locate(m, hsv, rgb);
            let c = field_color(m, z, x, y);
            assert_eq!(hex(c), hex(rgb), "mode {m}");
        }
    }

    #[test]
    fn hue_survives_grey_and_ok_sets_the_colour() {
        let mut f = Map::new();
        f.insert("color".into(), json!("#808080"));
        f.insert("__hsv".into(), json!([200.0, 0.0, 128.0 / 255.0]));
        assert_eq!(current(&f).1[0], 200.0);
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let id = open(&mut app, "background");
        let d = app.ui.dialog_mut(id).unwrap();
        d.fields.insert("color".into(), json!("#3366cc"));
        let fields = d.fields.clone();
        confirm(&mut app, &fields).unwrap();
        assert_eq!(hex([app.session.tools.background[0], app.session.tools.background[1], app.session.tools.background[2]]), "#3366cc");
    }
}
