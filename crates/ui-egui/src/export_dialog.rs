//! File › Export › Export As… (and Quick Export as PNG): format, quality, transparency and scale,
//! with a preview and an estimated file size. The estimate encodes a small proxy and scales by
//! pixel count, so the dialog stays instant on 36 MP documents.

use std::sync::Arc;

use photocraft_doc::Document;
use serde_json::{Map, Value, json};

use crate::state::DialogKind;
use crate::theme::Tokens;
use crate::{ExportSettings, PhotocraftApp};

const FORMATS: [(&str, &str); 4] = [("png", "PNG"), ("jpg", "JPG"), ("webp", "WebP (lossless)"), ("tif", "TIFF")];

pub fn open(app: &mut PhotocraftApp) -> Result<u64, String> {
    let st = app.session.active().ok_or("no document")?;
    let mut f = Map::new();
    f.insert("__export".into(), json!(true));
    f.insert("__label".into(), json!("Export As"));
    f.insert("format".into(), json!("png"));
    f.insert("quality".into(), json!(85));
    f.insert("transparency".into(), json!(true));
    f.insert("scale".into(), json!(100));
    f.insert("__w".into(), json!(st.doc.size.width));
    f.insert("__h".into(), json!(st.doc.size.height));
    Ok(app.ui.open_dialog(DialogKind::Command, f))
}

/// Layer › Export As…: the same dialog for just the active layer (trimmed to its pixels).
pub fn open_layer(app: &mut PhotocraftApp, layer: photocraft_doc::LayerId) -> Result<u64, String> {
    let st = app.session.active().ok_or("no document")?;
    let ldoc = photocraft_engine::layer_menu_cmds::layer_document(&st.doc, layer).map_err(|e| e.to_string())?;
    let id = open(app)?;
    if let Some(d) = app.ui.dialogs.iter_mut().find(|d| d.id == id) {
        d.fields.insert("__layer".into(), json!(layer.0));
        d.fields.insert("__label".into(), json!(format!("Export As: {}", ldoc.name)));
        d.fields.insert("__w".into(), json!(ldoc.size.width));
        d.fields.insert("__h".into(), json!(ldoc.size.height));
    }
    Ok(id)
}

/// Layer › Quick Export as PNG.
pub fn quick_export_layer_png(app: &mut PhotocraftApp, layer: photocraft_doc::LayerId) -> Result<Value, String> {
    let mut f = Map::new();
    f.insert("format".into(), json!("png"));
    f.insert("transparency".into(), json!(true));
    f.insert("scale".into(), json!(100));
    f.insert("__layer".into(), json!(layer.0));
    confirm(app, &f)
}

/// The document a dialog exports: the whole image, or one layer (`__layer`).
fn source_document(app: &PhotocraftApp, f: &Map<String, Value>) -> Result<Arc<Document>, String> {
    let st = app.session.active().ok_or("no document")?;
    match f.get("__layer").and_then(Value::as_u64) {
        Some(id) => photocraft_engine::layer_menu_cmds::layer_document(&st.doc, photocraft_doc::LayerId(id)).map(Arc::new).map_err(|e| e.to_string()),
        None => Ok(st.doc.clone()),
    }
}

fn s(f: &Map<String, Value>, k: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}
fn n(f: &Map<String, Value>, k: &str, d: f64) -> f64 {
    f.get(k).and_then(Value::as_f64).unwrap_or(d)
}

/// The document as exported: scaled (engine Image Size, bicubic) and flattened over white when
/// transparency is off or the format has no alpha.
fn export_document(doc: &Document, f: &Map<String, Value>, max_side: Option<u32>) -> Result<Document, String> {
    let mut s = photocraft_engine::Session::new();
    s.add_document(doc.clone(), None);
    let scale = n(f, "scale", 100.0) / 100.0;
    let mut w = (doc.size.width as f64 * scale).round().max(1.0);
    if let Some(m) = max_side {
        let long = doc.size.width.max(doc.size.height) as f64 * scale;
        if long > m as f64 {
            w = (w * m as f64 / long).round().max(1.0);
        }
    }
    if (w - doc.size.width as f64).abs() >= 1.0 {
        s.execute("image.imageSize", json!({"width": w, "resample": if max_side.is_some() { "bilinear" } else { "bicubic" }})).map_err(|e| e.to_string())?;
    }
    let fmt = s_fmt(f);
    if !f.get("transparency").and_then(Value::as_bool).unwrap_or(true) || fmt == "jpg" {
        s.execute("layer.flattenImage", json!({})).map_err(|e| e.to_string())?;
    }
    s.active().map(|d| (*d.doc).clone()).ok_or_else(|| "export failed".into())
}

fn s_fmt(f: &Map<String, Value>) -> String {
    let v = s(f, "format");
    if v.is_empty() { "png".into() } else { v }
}

fn settings(f: &Map<String, Value>) -> ExportSettings {
    ExportSettings { jpeg_quality: (s_fmt(f) == "jpg").then(|| n(f, "quality", 85.0).clamp(1.0, 100.0) as u8) }
}

/// Estimated size (bytes) from a ≤512 px proxy encode, scaled by pixel count.
fn estimate(app: &PhotocraftApp, doc: &Document, f: &Map<String, Value>) -> Option<u64> {
    let export = app.services.export.as_ref()?;
    let proxy = export_document(doc, f, Some(512)).ok()?;
    let (bytes, _) = export(&proxy, &format!("estimate.{}", s_fmt(f)), &settings(f)).ok()?;
    let scale = n(f, "scale", 100.0) / 100.0;
    let full = doc.size.width as f64 * scale * doc.size.height as f64 * scale;
    let small = (proxy.size.width as f64 * proxy.size.height as f64).max(1.0);
    Some((bytes.len() as f64 * full / small) as u64)
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let Some(doc) = app.session.active().map(|s| s.doc.clone()) else { return };
    ui.horizontal_top(|ui| {
        // Left: settings.
        ui.vertical(|ui| {
            ui.set_width(220.0);
            ui.label(egui::RichText::new(tl!("File Settings")).font(crate::theme::semibold(12.0)).color(t.text));
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Format")).color(t.text_dim));
                let mut fmt = s_fmt(f);
                let opts: Vec<(String, &str)> = FORMATS.iter().map(|(k, l)| (k.to_string(), *l)).collect();
                if crate::widgets::dropdown(ui, "export-format", &mut fmt, &opts, 130.0) {
                    f.insert("format".into(), json!(fmt));
                }
            });
            let fmt = s_fmt(f);
            if fmt == "jpg" {
                let mut q = n(f, "quality", 85.0) as f32;
                crate::widgets::slider_row(ui, tl!("Quality"), &mut q, 1.0..=100.0, "%", None);
                f.insert("quality".into(), json!(q.round()));
            } else {
                let mut tr = f.get("transparency").and_then(Value::as_bool).unwrap_or(true);
                crate::widgets::checkbox(ui, &mut tr, tl!("Transparency"));
                f.insert("transparency".into(), json!(tr));
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Image Size")).font(crate::theme::semibold(12.0)).color(t.text));
            let mut sc = n(f, "scale", 100.0) as f32;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Scale")).color(t.text_dim));
                crate::widgets::value_field(ui, &mut sc, 1.0..=1000.0, "%", 70.0);
            });
            f.insert("scale".into(), json!(sc.round()));
            let (bw, bh) = (n(f, "__w", f64::from(doc.size.width)) as f32, n(f, "__h", f64::from(doc.size.height)) as f32);
            let (w, h) = ((bw * sc / 100.0).round(), (bh * sc / 100.0).round());
            ui.label(egui::RichText::new(format!("{w} × {h} px")).color(t.text_dim).size(11.5));
        });
        ui.add_space(12.0);
        // Right: preview + estimated size.
        ui.vertical(|ui| {
            let layer = f.get("__layer").and_then(Value::as_u64);
            let key = egui::Id::new(("export-preview", doc.id.0, app.session.active().map_or(0, |s| s.revision), layer));
            let sig =
                format!("{}{}{}{}", s_fmt(f), n(f, "quality", 85.0), f.get("transparency").map(|v| v.to_string()).unwrap_or_default(), n(f, "scale", 100.0));
            let cached: Option<(String, Option<u64>, Arc<egui::TextureHandle>)> = ui.data(|d| d.get_temp(key));
            let (size, tex) = match cached.filter(|c| c.0 == sig) {
                Some((_, size, tex)) => (size, tex),
                None => {
                    let src = source_document(app, f).unwrap_or_else(|_| doc.clone());
                    let size = estimate(app, &src, f);
                    let img = photocraft_compose::thumbnail(&export_document(&src, f, Some(360)).unwrap_or_else(|_| (*src).clone()), 360);
                    let color = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels);
                    let tex = Arc::new(ui.ctx().load_texture("export-preview", color, egui::TextureOptions::LINEAR));
                    ui.data_mut(|d| d.insert_temp(key, (sig, size, tex.clone())));
                    (size, tex)
                }
            };
            let side = 300.0;
            let (r, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
            crate::widgets::checker(ui.painter(), r, 8.0);
            let ts = tex.size_vec2();
            let k = (side / ts.x.max(ts.y)).min(1.0e3);
            let ir = egui::Rect::from_center_size(r.center(), ts * k);
            ui.painter().image(tex.id(), ir, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Outside);
            let est = size.map_or("—".to_string(), |b| crate::sizing::human_bytes(b as f64));
            ui.label(egui::RichText::new(format!("{}  ≈ {est}", s_fmt(f).to_uppercase())).color(t.text_dim).size(11.5));
        });
    });
}

/// Export with the dialog's settings: choose a path, render, encode, write.
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let doc = source_document(app, f)?;
    let stem = doc.name.rsplit_once('.').map_or(doc.name.as_str(), |(a, _)| a).to_string();
    let ext = s_fmt(f);
    let suggested = format!("{stem}.{ext}");
    let path = app.services.pick_save.as_mut().and_then(|p| p(&suggested)).ok_or("cancelled")?;
    let out = export_document(&doc, f, None)?;
    let export = app.services.export.as_ref().ok_or("no exporter configured")?;
    let (bytes, warnings) = export(&out, &path, &settings(f))?;
    let write = app.services.write.as_mut().ok_or("no writer configured")?;
    write(&path, &bytes)?;
    app.ui.status = format!("Exported {path} ({})", crate::sizing::human_bytes(bytes.len() as f64));
    app.ui.status_error = false;
    crate::notices::io_warnings(app, &format!("Exported {}", crate::file_open::display_name(&path)), &warnings);
    Ok(json!({"path": path, "bytes": bytes.len(), "warnings": warnings}))
}

/// File › Export › Quick Export as PNG: the format, quality, metadata, colour space and location
/// from File › Export › Export Preferences (engine `file.export.quickExport`). On the web (no
/// file system) it falls back to a PNG download through the export service.
pub fn quick_export_png(app: &mut PhotocraftApp) -> Result<Value, String> {
    if !cfg!(target_arch = "wasm32") {
        let prefs = app.session.prefs().export.clone();
        let fmt = serde_json::to_value(prefs.quick_export_format).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "png".into());
        let same = serde_json::to_value(prefs.quick_export_location).ok().is_some_and(|v| v == "sameFolder");
        let saved = app.session.active().is_some_and(|d| d.path.is_some());
        let p = if same && saved {
            json!({})
        } else {
            let st = app.session.active().ok_or("no document")?;
            let stem = st.doc.name.rsplit_once('.').map_or(st.doc.name.as_str(), |(a, _)| a).to_string();
            let path = app.services.pick_save.as_mut().and_then(|p| p(&format!("{stem}.{fmt}"))).ok_or("cancelled")?;
            json!({"path": path})
        };
        let r = app.run("file.export.quickExport", p)?;
        app.ui.status = format!("Exported {}", r["path"].as_str().unwrap_or_default());
        return Ok(r);
    }
    let mut f = Map::new();
    f.insert("format".into(), json!("png"));
    f.insert("transparency".into(), json!(true));
    f.insert("scale".into(), json!(100));
    confirm(app, &f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_document_scales_and_flattens() {
        let doc = Document::with_background(
            "x",
            photocraft_doc::Size::new(200, 100),
            photocraft_doc::ColorMode::Rgb,
            photocraft_doc::SampleType::U8,
            photocraft_doc::Color::WHITE,
        );
        let mut f = Map::new();
        f.insert("format".into(), json!("jpg"));
        f.insert("scale".into(), json!(50));
        let out = export_document(&doc, &f, None).unwrap();
        assert_eq!((out.size.width, out.size.height), (100, 50));
        assert_eq!(out.layers.len(), 1);
        let proxy = export_document(&doc, &f, Some(40)).unwrap();
        assert_eq!(proxy.size.width, 40);
        assert_eq!(settings(&f).jpeg_quality, Some(85));
    }
}
