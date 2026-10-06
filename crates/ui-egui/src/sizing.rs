//! Photoshop-style Image Size and Canvas Size dialogs.
//!
//! Field values are stored in pixels (plus a display unit), so automation can set `width`/`height`
//! directly and the dialog maps them to `image.imageSize` / `image.canvasSize` unchanged.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;
use crate::theme::Tokens;

const UNITS: [(&str, &str); 7] =
    [("px", "Pixels"), ("percent", "Percent"), ("in", "Inches"), ("cm", "Centimeters"), ("mm", "Millimeters"), ("pt", "Points"), ("pica", "Picas")];

/// Dialog unit for the Units & Rulers preference.
fn pref_unit(u: photocraft_engine::prefs::Unit) -> &'static str {
    use photocraft_engine::prefs::Unit;
    match u {
        Unit::Pixels => "px",
        Unit::Percent => "percent",
        Unit::Inches => "in",
        Unit::Centimeters => "cm",
        Unit::Millimeters => "mm",
        Unit::Points => "pt",
        Unit::Picas => "pica",
    }
}

/// Image Size resampling for Preferences › General › Image Interpolation.
fn pref_resample(i: photocraft_engine::prefs::Interpolation) -> &'static str {
    use photocraft_engine::prefs::Interpolation;
    match i {
        Interpolation::Nearest => "nearest",
        Interpolation::Bilinear => "bilinear",
        Interpolation::BicubicSharper => "lanczos",
        Interpolation::PreserveDetails => "preserveDetails",
        _ => "bicubic",
    }
}
const ANCHORS: [&str; 9] = ["topLeft", "top", "topRight", "left", "center", "right", "bottomLeft", "bottom", "bottomRight"];

pub fn is_sizing(command: &str) -> bool {
    matches!(command, "image.imageSize" | "image.canvasSize")
}

pub fn open(app: &mut PhotocraftApp, command: &str) -> Option<u64> {
    let unit = pref_unit(app.session.prefs().units_and_rulers.rulers);
    let resample = pref_resample(app.session.prefs().general.image_interpolation);
    let d = &app.session.active()?.doc;
    let (w, h, res) = (d.size.width as f64, d.size.height as f64, d.resolution_dpi as f64);
    let label = photocraft_engine::commands::find(command).map(|c| c.label).unwrap_or("Image Size…");
    let mut f = Map::new();
    f.insert("__command".into(), json!(command));
    f.insert("__label".into(), json!(label));
    f.insert("__sizing".into(), json!(true));
    f.insert("__origW".into(), json!(w));
    f.insert("__origH".into(), json!(h));
    f.insert("__origRes".into(), json!(res));
    f.insert("__unit".into(), json!(unit));
    f.insert("__bytesPerPixel".into(), json!(d.layers.first().and_then(|l| l.surface()).map_or(4, |s| s.format().bytes_per_pixel())));
    f.insert("width".into(), json!(w));
    f.insert("height".into(), json!(h));
    if command == "image.imageSize" {
        f.insert("__constrain".into(), json!(true));
        f.insert("resolution".into(), json!(res));
        f.insert("resample".into(), json!(resample));
    } else {
        f.insert("relative".into(), json!(false));
        f.insert("anchor".into(), json!("center"));
        f.insert("extensionColor".into(), json!("background"));
    }
    Some(app.ui.open_dialog(DialogKind::Command, f))
}

fn num(f: &Map<String, Value>, k: &str) -> f64 {
    f.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

/// Pixels → display unit.
fn to_unit(px: f64, unit: &str, orig: f64, res: f64) -> f64 {
    match unit {
        "percent" => px / orig.max(1.0) * 100.0,
        "in" => px / res.max(1.0),
        "cm" => px / res.max(1.0) * 2.54,
        "mm" => px / res.max(1.0) * 25.4,
        "pt" => px / res.max(1.0) * 72.0,
        "pica" => px / res.max(1.0) * 6.0,
        _ => px,
    }
}

fn from_unit(v: f64, unit: &str, orig: f64, res: f64) -> f64 {
    match unit {
        "percent" => v * orig / 100.0,
        "in" => v * res,
        "cm" => v / 2.54 * res,
        "mm" => v / 25.4 * res,
        "pt" => v / 72.0 * res,
        "pica" => v / 6.0 * res,
        _ => v,
    }
}

pub fn human_bytes(b: f64) -> String {
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2}G", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1}M", b / (1024.0 * 1024.0))
    } else {
        format!("{:.0}K", (b / 1024.0).max(1.0))
    }
}

pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    if f.get("__command").and_then(Value::as_str) == Some("image.canvasSize") {
        canvas_size(ui, f);
    } else {
        image_size(ui, f);
    }
}

fn label(ui: &mut egui::Ui, text: &str, width: f32) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
    ui.painter().text(pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), t.text_dim);
}

fn info_row(ui: &mut egui::Ui, k: &str, v: String) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        label(ui, tl!(k), 90.0);
        ui.label(egui::RichText::new(v).color(t.text).size(12.0));
    });
}

/// Width / height field in the current unit, writing back pixels. Returns true when edited.
fn dim_field(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, orig_key: &str, id: &str) -> bool {
    let unit = f.get("__unit").and_then(Value::as_str).unwrap_or("px").to_string();
    let res = f.get("resolution").and_then(Value::as_f64).unwrap_or(num(f, "__origRes"));
    let orig = num(f, orig_key);
    let px = num(f, key);
    let mut v = to_unit(px, &unit, orig, res) as f32;
    let range = if unit == "px" { -300000.0..=300000.0 } else { -30000.0..=30000.0 };
    let changed = crate::widgets::value_field(ui, &mut v, range, "", 90.0).changed();
    if changed {
        let px = from_unit(v as f64, &unit, orig, res);
        f.insert(key.into(), json!(if unit == "px" { px.round() } else { px }));
    }
    let _ = id;
    changed
}

fn unit_dropdown(ui: &mut egui::Ui, f: &mut Map<String, Value>, id: &str) {
    let mut unit = f.get("__unit").and_then(Value::as_str).unwrap_or("px").to_string();
    let opts: Vec<(String, &str)> = UNITS.iter().map(|(k, l)| (k.to_string(), *l)).collect();
    if crate::widgets::dropdown(ui, id, &mut unit, &opts, 110.0) {
        f.insert("__unit".into(), json!(unit));
    }
}

fn image_size(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let (ow, oh) = (num(f, "__origW"), num(f, "__origH"));
    let bpp = num(f, "__bytesPerPixel").max(1.0);
    let resample = f.get("resample").and_then(Value::as_str).unwrap_or("bicubic").to_string();
    let resampling = resample != "none";
    let (w, h) = (num(f, "width").round(), num(f, "height").round());
    info_row(ui, tl!("Image Size:"), format!("{} (was {})", human_bytes(w * h * bpp), human_bytes(ow * oh * bpp)));
    info_row(ui, tl!("Dimensions:"), format!("{} px × {} px", w as i64, h as i64));
    ui.add_space(6.0);
    let mut constrain = f.get("__constrain").and_then(Value::as_bool).unwrap_or(true);
    let w_row = ui.horizontal(|ui| {
        label(ui, tl!("Width:"), 90.0);
        let c = dim_field(ui, f, "width", "__origW", "is-w");
        unit_dropdown(ui, f, "is-unit-w");
        c
    });
    let h_row = ui.horizontal(|ui| {
        label(ui, tl!("Height:"), 90.0);
        let c = dim_field(ui, f, "height", "__origH", "is-h");
        unit_dropdown(ui, f, "is-unit-h");
        c
    });
    let (w_changed, h_changed) = (w_row.inner, h_row.inner);
    // Photoshop's chain bracket linking width and height (right of the unit dropdowns).
    let x0 = w_row.response.rect.right().max(h_row.response.rect.right()) + 4.0;
    let bracket = Rect::from_min_max(pos2(x0, w_row.response.rect.center().y), pos2(x0 + 14.0, h_row.response.rect.center().y));
    let link = ui.interact(bracket.expand(2.0), ui.id().with("constrain"), Sense::click());
    let col = if constrain { t.text } else { t.text_faint };
    if constrain {
        let p = ui.painter();
        let x = bracket.left() + 8.0;
        p.line_segment([pos2(bracket.left(), bracket.top()), pos2(x, bracket.top())], Stroke::new(1.0, col));
        p.line_segment([pos2(x, bracket.top()), pos2(x, bracket.center().y - 8.0)], Stroke::new(1.0, col));
        p.line_segment([pos2(x, bracket.center().y + 8.0), pos2(x, bracket.bottom())], Stroke::new(1.0, col));
        p.line_segment([pos2(x, bracket.bottom()), pos2(bracket.left(), bracket.bottom())], Stroke::new(1.0, col));
    }
    let chain = Rect::from_center_size(pos2(bracket.left() + 8.0, bracket.center().y), vec2(14.0, 14.0));
    ui.painter().rect_filled(chain, 2.0, if link.hovered() { t.hover } else { Color32::TRANSPARENT });
    crate::icons::paint(ui, chain, if constrain { "link" } else { "unlink" }, 11.0, col);
    if link.on_hover_text(tl!("Constrain proportions")).clicked() {
        constrain = !constrain;
        f.insert("__constrain".into(), json!(constrain));
    }
    if constrain && resampling {
        if w_changed {
            f.insert("height".into(), json!((num(f, "width") * oh / ow.max(1.0)).round().max(1.0)));
        } else if h_changed {
            f.insert("width".into(), json!((num(f, "height") * ow / oh.max(1.0)).round().max(1.0)));
        }
    }
    ui.horizontal(|ui| {
        label(ui, tl!("Resolution:"), 90.0);
        let mut r = f.get("resolution").and_then(Value::as_f64).unwrap_or(72.0) as f32;
        if crate::widgets::value_field(ui, &mut r, 1.0..=30000.0, "", 90.0).changed() {
            let r = r.max(1.0) as f64;
            if !resampling {
                // Without resampling the pixel count is fixed: physical size follows resolution.
                f.insert("width".into(), json!(ow));
                f.insert("height".into(), json!(oh));
            }
            f.insert("resolution".into(), json!(r));
        }
        ui.label(egui::RichText::new(tl!("Pixels/Inch")).color(t.text_dim).size(12.0));
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        label(ui, "", 90.0);
        let mut on = resampling;
        if crate::widgets::checkbox(ui, &mut on, tl!("Resample")).changed() {
            f.insert("resample".into(), json!(if on { "bicubic" } else { "none" }));
            if !on {
                f.insert("width".into(), json!(ow));
                f.insert("height".into(), json!(oh));
            }
        }
        if resampling {
            let mut cur = resample.clone();
            let opts = [
                ("preserveDetails".to_string(), tl!("Preserve Details")),
                ("bicubic".to_string(), tl!("Bicubic (smooth gradients)")),
                ("lanczos".to_string(), tl!("Lanczos (sharp)")),
                ("bilinear".to_string(), tl!("Bilinear")),
                ("nearest".to_string(), tl!("Nearest Neighbor (hard edges)")),
            ];
            if crate::widgets::dropdown(ui, "is-resample", &mut cur, &opts, 200.0) {
                f.insert("resample".into(), json!(cur));
            }
        }
    });
}

fn canvas_size(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let (ow, oh) = (num(f, "__origW"), num(f, "__origH"));
    let bpp = num(f, "__bytesPerPixel").max(1.0);
    let relative = f.get("relative").and_then(Value::as_bool).unwrap_or(false);
    ui.label(egui::RichText::new(tl!("Current Size")).font(crate::theme::semibold(12.0)).color(t.text));
    info_row(ui, tl!("Size:"), human_bytes(ow * oh * bpp));
    info_row(ui, tl!("Width:"), format!("{} px", ow as i64));
    info_row(ui, tl!("Height:"), format!("{} px", oh as i64));
    ui.add_space(6.0);
    crate::widgets::hairline(ui);
    ui.add_space(6.0);
    let (nw, nh) = if relative { (ow + num(f, "width"), oh + num(f, "height")) } else { (num(f, "width"), num(f, "height")) };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("New Size")).font(crate::theme::semibold(12.0)).color(t.text));
        ui.label(egui::RichText::new(human_bytes(nw.max(0.0) * nh.max(0.0) * bpp)).color(t.text_dim).size(12.0));
    });
    ui.horizontal(|ui| {
        label(ui, tl!("Width:"), 90.0);
        dim_field(ui, f, "width", "__origW", "cs-w");
        unit_dropdown(ui, f, "cs-unit-w");
    });
    ui.horizontal(|ui| {
        label(ui, tl!("Height:"), 90.0);
        dim_field(ui, f, "height", "__origH", "cs-h");
        unit_dropdown(ui, f, "cs-unit-h");
    });
    ui.horizontal(|ui| {
        label(ui, "", 90.0);
        let mut rel = relative;
        if crate::widgets::checkbox(ui, &mut rel, tl!("Relative")).changed() {
            // Convert between absolute and delta so the resulting canvas is unchanged.
            let (w, h) = (num(f, "width"), num(f, "height"));
            let (w, h) = if rel { (w - ow, h - oh) } else { (w + ow, h + oh) };
            f.insert("width".into(), json!(w));
            f.insert("height".into(), json!(h));
            f.insert("relative".into(), json!(rel));
        }
    });
    ui.horizontal(|ui| {
        label(ui, tl!("Anchor:"), 90.0);
        let cur = f.get("anchor").and_then(Value::as_str).unwrap_or("center").to_string();
        if let Some(a) = anchor_grid(ui, &cur, nw - ow, nh - oh) {
            f.insert("anchor".into(), json!(a));
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        label(ui, tl!("Canvas extension color:"), 150.0);
        let mut cur = f.get("extensionColor").and_then(Value::as_str).unwrap_or("background").to_string();
        let opts = [
            ("foreground".to_string(), tl!("Foreground")),
            ("background".to_string(), tl!("Background")),
            ("white".to_string(), tl!("White")),
            ("black".to_string(), tl!("Black")),
            ("transparent".to_string(), tl!("Transparent")),
        ];
        if crate::widgets::dropdown(ui, "cs-ext", &mut cur, &opts, 140.0) {
            f.insert("extensionColor".into(), json!(cur));
        }
    });
}

/// 3×3 anchor picker; arrows point away from the anchor in the directions the canvas grows.
fn anchor_grid(ui: &mut egui::Ui, current: &str, dw: f64, dh: f64) -> Option<&'static str> {
    let t = Tokens::get(ui.ctx());
    let cell = 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(cell * 3.0, cell * 3.0), Sense::hover());
    let ci = ANCHORS.iter().position(|a| *a == current).unwrap_or(4) as i32;
    let (ax, ay) = (ci % 3, ci / 3);
    let mut picked = None;
    for (i, a) in ANCHORS.iter().enumerate() {
        let (x, y) = (i as i32 % 3, i as i32 / 3);
        let r = Rect::from_min_size(rect.min + vec2(x as f32 * cell, y as f32 * cell), vec2(cell, cell)).shrink(1.0);
        let resp = ui.interact(r, ui.id().with(("anchor", i)), Sense::click());
        let selected = i as i32 == ci;
        ui.painter().rect_filled(
            r,
            2.0,
            if selected {
                t.field.gamma_multiply(1.6)
            } else if resp.hovered() {
                t.hover
            } else {
                t.field
            },
        );
        ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, t.separator), StrokeKind::Inside);
        if selected {
            ui.painter().rect_filled(Rect::from_center_size(r.center(), vec2(8.0, 8.0)), 1.0, t.text);
        } else {
            let (dx, dy) = (x - ax, y - ay);
            // Only neighbouring cells show arrows (Photoshop behaviour); direction flips when shrinking.
            if dx.abs() <= 1 && dy.abs() <= 1 {
                let sx = if dw < 0.0 { -1.0 } else { 1.0 };
                let sy = if dh < 0.0 { -1.0 } else { 1.0 };
                let dir = vec2(dx as f32 * sx, dy as f32 * sy);
                if dir != egui::Vec2::ZERO {
                    let dir = dir.normalized();
                    let c = r.center();
                    let tip = c + dir * 6.0;
                    let tail = c - dir * 5.0;
                    let side = vec2(-dir.y, dir.x) * 3.5;
                    let col = t.text_dim;
                    ui.painter().line_segment([tail, tip], Stroke::new(1.3, col));
                    ui.painter().add(egui::Shape::convex_polygon(vec![tip + dir * 1.5, tip - dir * 3.5 + side, tip - dir * 3.5 - side], col, Stroke::NONE));
                }
            }
        }
        if resp.clicked() {
            picked = Some(*a);
        }
    }
    let _ = Color32::WHITE;
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversions_round_trip() {
        for unit in ["px", "percent", "in", "cm", "mm", "pt", "pica"] {
            let v = to_unit(1500.0, unit, 3000.0, 300.0);
            assert!((from_unit(v, unit, 3000.0, 300.0) - 1500.0).abs() < 1e-9, "{unit}");
        }
        assert_eq!(to_unit(1500.0, "percent", 3000.0, 300.0), 50.0);
        assert_eq!(to_unit(600.0, "in", 3000.0, 300.0), 2.0);
        assert_eq!(human_bytes(6016.0 * 6016.0 * 4.0), "138.1M");
    }

    #[test]
    fn dialog_maps_to_engine_command() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 200, "height": 100})).unwrap();
        let id = open(&mut app, "image.imageSize").unwrap();
        let d = app.ui.dialog_mut(id).unwrap();
        d.fields.insert("width".into(), json!(100));
        d.fields.insert("height".into(), json!(50));
        crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(app.session.active().unwrap().doc.size, photocraft_doc::Size::new(100, 50));
        let id = open(&mut app, "image.canvasSize").unwrap();
        let d = app.ui.dialog_mut(id).unwrap();
        d.fields.insert("width".into(), json!(120));
        d.fields.insert("anchor".into(), json!("topLeft"));
        crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(app.session.active().unwrap().doc.size, photocraft_doc::Size::new(120, 50));
    }
}
