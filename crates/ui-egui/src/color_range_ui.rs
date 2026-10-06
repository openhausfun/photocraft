//! Select › Color Range… as Photoshop lays it out: the Select menu (Sampled Colors, the six hue
//! families, Highlights / Midtones / Shadows, Out Of Gamut), Fuzziness, Localized Color Clusters
//! with Range, the tonal range of Highlights / Midtones / Shadows, the three eyedroppers (sample,
//! add, subtract; they pick on the preview, Shift adds and Alt subtracts), a Selection / Image
//! preview and Invert. Controls a mode doesn't use are disabled, like Photoshop's.
//!
//! Everything lives in the dialog fields (`ui.dialog.set` drives it). The preview and OK run the
//! same engine command, `select.colorRange`, so the GUI, MCP and scripts select identically; the
//! preview runs it on a small proxy of the document, OK on the document itself (one history step).
//! Cancel never touches the document, so the previous selection stays as it was.

use std::sync::Arc;

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_doc::{DocId, Document};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;
use crate::theme::Tokens;
use crate::widgets;

/// The engine command the dialog drives.
pub const COMMAND: &str = "select.colorRange";

/// Select menu entries in Photoshop's order (engine value, label). Skin Tones is left out: the
/// engine has no skin-tone model.
pub const SELECTS: &[(&str, &str)] = &[
    ("sampledColors", "Sampled Colors"),
    ("reds", "Reds"),
    ("yellows", "Yellows"),
    ("greens", "Greens"),
    ("cyans", "Cyans"),
    ("blues", "Blues"),
    ("magentas", "Magentas"),
    ("highlights", "Highlights"),
    ("midtones", "Midtones"),
    ("shadows", "Shadows"),
    ("outOfGamut", "Out Of Gamut"),
];

/// Longest side of the preview thumbnail, in points.
const PREVIEW: u32 = 200;

/// Which controls the current Select mode uses (the rest are disabled or hidden).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    /// Fuzziness (Sampled Colors: colour distance; tones: falloff in % of the tonal scale).
    pub fuzziness: bool,
    /// The eyedroppers and Localized Color Clusters (Sampled Colors only).
    pub sampling: bool,
    /// The Range slider (Localized Color Clusters on).
    pub range: bool,
    /// The tonal range sliders (Highlights / Midtones / Shadows).
    pub tonal: bool,
}

fn s<'a>(f: &'a Map<String, Value>, k: &str, d: &'a str) -> &'a str {
    f.get(k).and_then(Value::as_str).unwrap_or(d)
}
fn num(f: &Map<String, Value>, k: &str, d: f32) -> f32 {
    f.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}
fn flag(f: &Map<String, Value>, k: &str) -> bool {
    f.get(k).and_then(Value::as_bool).unwrap_or(false)
}
fn points(f: &Map<String, Value>, k: &str) -> Vec<[f64; 2]> {
    let pt = |v: &Value| match v.as_array()?.as_slice() {
        [x, y] => Some([x.as_f64()?, y.as_f64()?]),
        _ => None,
    };
    f.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(pt).collect()).unwrap_or_default()
}

pub fn controls(f: &Map<String, Value>) -> Controls {
    let select = s(f, "select", "sampledColors");
    let sampled = select == "sampledColors";
    let tonal = matches!(select, "highlights" | "midtones" | "shadows");
    Controls { fuzziness: sampled || tonal, sampling: sampled, range: sampled && flag(f, "localized"), tonal }
}

/// Open the dialog with Photoshop's defaults (Sampled Colors, Fuzziness 40, the foreground colour
/// as the sample until the eyedropper picks one).
pub fn open(app: &mut PhotocraftApp) -> u64 {
    let label = photocraft_engine::commands::find(COMMAND).map_or(tl!("Color Range…"), |c| c.label);
    let mut f = Map::new();
    f.insert("__colorRange".into(), json!(true));
    f.insert("__label".into(), json!(label));
    f.insert("select".into(), json!("sampledColors"));
    f.insert("fuzziness".into(), json!(40.0));
    f.insert("localized".into(), json!(false));
    f.insert("range".into(), json!(100.0));
    f.insert("toneFuzziness".into(), json!(20.0));
    f.insert("shadowsLevel".into(), json!(65.0));
    f.insert("highlightsLevel".into(), json!(190.0));
    f.insert("midtonesLow".into(), json!(105.0));
    f.insert("midtonesHigh".into(), json!(150.0));
    f.insert("invert".into(), json!(false));
    f.insert("points".into(), json!([]));
    f.insert("subtractPoints".into(), json!([]));
    f.insert("__tool".into(), json!("sample"));
    f.insert("__view".into(), json!("selection"));
    app.color_range = None;
    app.ui.open_dialog(DialogKind::Command, f)
}

pub fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key("__colorRange")
}

/// The `select.colorRange` params for the dialog's state: only what the Select mode uses.
pub fn params(f: &Map<String, Value>) -> Value {
    let select = s(f, "select", "sampledColors");
    let c = controls(f);
    let mut p = Map::new();
    p.insert("select".into(), json!(select));
    p.insert("invert".into(), json!(flag(f, "invert")));
    if c.sampling {
        p.insert("fuzziness".into(), json!(num(f, "fuzziness", 40.0)));
        let (add, sub) = (points(f, "points"), points(f, "subtractPoints"));
        let picked = !add.is_empty() || !sub.is_empty();
        if !add.is_empty() {
            p.insert("points".into(), json!(add));
        }
        if !sub.is_empty() {
            p.insert("subtractPoints".into(), json!(sub));
        }
        // Localized clusters need a picked position; until then it is the plain colour range.
        if c.range && picked {
            p.insert("localized".into(), json!(true));
            p.insert("range".into(), json!(num(f, "range", 100.0)));
        }
    } else if c.tonal {
        p.insert("fuzziness".into(), json!(num(f, "toneFuzziness", 20.0)));
        let range = match select {
            "shadows" => json!(num(f, "shadowsLevel", 65.0)),
            "highlights" => json!(num(f, "highlightsLevel", 190.0)),
            _ => {
                let (lo, hi) = (num(f, "midtonesLow", 105.0), num(f, "midtonesHigh", 150.0));
                json!([lo.min(hi), lo.max(hi)])
            }
        };
        p.insert("tonalRange".into(), range);
    }
    Value::Object(p)
}

/// OK: run the command on the document (one history step).
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    app.color_range = None;
    app.run(COMMAND, params(f))
}

/// Cached preview: the document proxy (per revision) and the textures drawn from it.
pub struct Preview {
    doc: DocId,
    revision: u64,
    /// Proxy scale: proxy pixel = `k` document pixels.
    k: u32,
    proxy: Arc<Document>,
    image: Option<egui::TextureHandle>,
    /// Hash of the params the mask texture shows.
    mask_key: u64,
    mask: Option<egui::TextureHandle>,
    /// Why the preview couldn't be drawn (shown in the dialog and logged, never silent: #145).
    error: Option<String>,
}

fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// The selection mask `params` would make, on the proxy: the engine command run on a scratch
/// session (Out Of Gamut uses the document's own proof setup).
fn proxy_mask(app: &PhotocraftApp, proxy: &Document, k: u32, params: &Value) -> Result<Vec<f32>, String> {
    let area = proxy.bounds();
    if params.get("select").and_then(Value::as_str) == Some("outOfGamut") {
        let pv = app.session.color.proof(proxy.id);
        let (m, _) = photocraft_engine::color_cmds::gamut_mask(proxy, &pv.setup, pv.gamut_threshold).map_err(|e| e.to_string())?;
        let invert = params.get("invert").and_then(Value::as_bool).unwrap_or(false);
        return Ok(m.iter().map(|v| f32::from(*v) / 255.0).map(|v| if invert { 1.0 - v } else { v }).collect());
    }
    let mut p = params.clone();
    // Eyedropper points are in document pixels; the proxy is 1/k the size.
    let (w, h) = (f64::from(area.width().max(1)), f64::from(area.height().max(1)));
    if let Some(o) = p.as_object_mut() {
        for key in ["points", "subtractPoints"] {
            if let Some(Value::Array(a)) = o.get_mut(key) {
                for v in a.iter_mut() {
                    if let Some([x, y]) = v.as_array().map(Vec::as_slice).and_then(|s| match s {
                        [x, y] => Some([x.as_f64()?, y.as_f64()?]),
                        _ => None,
                    }) {
                        *v = json!([(x / f64::from(k)).floor().clamp(0.0, w - 1.0), (y / f64::from(k)).floor().clamp(0.0, h - 1.0)]);
                    }
                }
            }
        }
    }
    let mut s = photocraft_engine::Session::new();
    s.tools = app.session.tools.clone();
    let mut doc = proxy.clone();
    doc.selection = None;
    s.add_document(doc, None);
    if let Some(id) = app.session.active().and_then(|d| d.active_layer) {
        let _ = s.select_layer(id);
    }
    s.execute(COMMAND, p).map_err(|e| e.to_string())?;
    let d = s.active().ok_or("no preview document")?;
    Ok(photocraft_algo::selection::mask_from_surface(d.doc.selection.as_ref(), area))
}

/// Record why the preview failed (logged once per new reason), or clear it.
fn report(app: &mut PhotocraftApp, error: Option<String>) {
    let Some(p) = app.color_range.as_mut() else { return };
    if p.error != error {
        if let Some(e) = &error {
            log::warn!("Color Range preview: {e}");
        }
        p.error = error;
    }
}

/// Refresh the cached proxy / textures for the active document and the dialog's params. Returns
/// (texture to show, its size in proxy pixels, k).
fn preview(app: &mut PhotocraftApp, ctx: &egui::Context, f: &Map<String, Value>) -> Option<(egui::TextureId, [usize; 2], u32)> {
    let (doc_id, revision, doc) = {
        let st = app.session.active()?;
        (st.doc.id, st.revision, st.doc.clone())
    };
    if !matches!(&app.color_range, Some(p) if p.doc == doc_id && p.revision == revision) {
        let side = doc.size.width.max(doc.size.height).max(1);
        let k = side.div_ceil(PREVIEW).max(1);
        let proxy = Arc::new(crate::proxy::proxy_document(&doc, k));
        app.color_range = Some(Preview { doc: doc_id, revision, k, proxy, image: None, mask_key: 0, mask: None, error: None });
    }
    let image_view = s(f, "__view", "selection") == "image";
    let params = params(f);
    let key = hash(&params.to_string()).max(1);
    let (proxy, k) = {
        let p = app.color_range.as_ref()?;
        (p.proxy.clone(), p.k)
    };
    let (w, h) = (proxy.size.width as usize, proxy.size.height as usize);
    if image_view {
        if app.color_range.as_ref().is_some_and(|p| p.image.is_none()) {
            let thumb = photocraft_compose::thumbnail(&proxy, proxy.size.width.max(proxy.size.height));
            let size = [thumb.width as usize, thumb.height as usize];
            if size != [w, h] || thumb.pixels.len() != w * h * 4 {
                report(app, Some(format!("the image thumbnail is {}×{}, expected {w}×{h}", thumb.width, thumb.height)));
                return None;
            }
            let img = egui::ColorImage::from_rgba_unmultiplied(size, &thumb.pixels);
            let tex = ctx.load_texture("color-range-image", img, egui::TextureOptions::LINEAR);
            if let Some(p) = app.color_range.as_mut() {
                p.image = Some(tex);
            }
        }
        return app.color_range.as_ref()?.image.as_ref().map(|t| (t.id(), [w, h], k));
    }
    if app.color_range.as_ref().is_some_and(|p| p.mask_key != key || p.mask.is_none()) {
        let mask = match proxy_mask(app, &proxy, k, &params) {
            Ok(m) => {
                report(app, None);
                m
            }
            Err(e) => {
                report(app, Some(e));
                vec![0.0; w * h]
            }
        };
        if mask.len() != w * h {
            report(app, Some(format!("the selection preview has {} pixels, expected {}", mask.len(), w * h)));
            return None;
        }
        let px: Vec<Color32> = mask.iter().map(|v| Color32::from_gray((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)).collect();
        let img = egui::ColorImage::new([w, h], px);
        let p = app.color_range.as_mut()?;
        match &mut p.mask {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => p.mask = Some(ctx.load_texture("color-range-mask", img, egui::TextureOptions::LINEAR)),
        }
        p.mask_key = key;
    }
    app.color_range.as_ref()?.mask.as_ref().map(|t| (t.id(), [w, h], k))
}

/// Photoshop-style radio button.
fn radio(ui: &mut egui::Ui, on: bool, label: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
        ui.painter().circle_stroke(r.center(), 5.5, Stroke::new(1.2, if on { t.accent } else { t.text_faint }));
        if on {
            ui.painter().circle_filled(r.center(), 3.0, t.accent);
        }
        let l = ui.add(egui::Label::new(egui::RichText::new(tl!(&label)).color(t.text_dim)).sense(Sense::click()));
        resp.union(l)
    })
    .inner
}

/// Eyedropper button: the pipette icon with a +/− badge; labelled for accessibility.
fn eyedropper(ui: &mut egui::Ui, badge: &str, selected: bool, label: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let r = crate::icons::button(ui, "pipette", 26.0, selected, label);
    if !badge.is_empty() {
        let at = r.rect.right_bottom() - vec2(5.0, 6.0);
        ui.painter().text(at, egui::Align2::CENTER_CENTER, badge, crate::theme::semibold(12.0), if selected { t.accent_text } else { t.text });
    }
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, label));
    r
}

fn set_points(f: &mut Map<String, Value>, k: &str, pts: &[[f64; 2]]) {
    f.insert(k.into(), json!(pts));
}

/// Dialog body.
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let c = controls(f);
    let select = s(f, "select", "sampledColors").to_string();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Select:")).color(t.text_dim));
        let mut cur: &str = &select;
        if widgets::dropdown(ui, "color-range-select", &mut cur, SELECTS, 170.0) {
            f.insert("select".into(), json!(cur));
        }
    });
    ui.add_space(4.0);
    ui.add_enabled_ui(c.sampling, |ui| {
        let mut on = flag(f, "localized");
        if widgets::checkbox(ui, &mut on, tl!("Localized Color Clusters")).changed() {
            f.insert("localized".into(), json!(on));
        }
    });
    ui.add_space(4.0);
    ui.add_enabled_ui(c.fuzziness, |ui| {
        if c.tonal {
            let mut v = num(f, "toneFuzziness", 20.0);
            if widgets::slider_row(ui, tl!("Fuzziness:"), &mut v, 0.0..=100.0, "%", None).changed() {
                f.insert("toneFuzziness".into(), json!(v.round()));
            }
        } else {
            let mut v = num(f, "fuzziness", 40.0);
            if widgets::slider_row(ui, tl!("Fuzziness:"), &mut v, 0.0..=200.0, "", None).changed() {
                f.insert("fuzziness".into(), json!(v.round()));
            }
        }
    });
    if c.range {
        let mut v = num(f, "range", 100.0);
        if widgets::slider_row(ui, tl!("Range:"), &mut v, 0.0..=100.0, "%", None).changed() {
            f.insert("range".into(), json!(v.round()));
        }
    }
    if c.tonal {
        let mut level = |ui: &mut egui::Ui, key: &str, label: &str, d: f32| {
            let mut v = num(f, key, d);
            if widgets::slider_row(ui, label, &mut v, 0.0..=255.0, "", None).changed() {
                f.insert(key.into(), json!(v.round()));
            }
        };
        match select.as_str() {
            "shadows" => level(ui, "shadowsLevel", "Shadows up to:", 65.0),
            "highlights" => level(ui, "highlightsLevel", "Highlights from:", 190.0),
            _ => {
                level(ui, "midtonesLow", tl!("Midtones from:"), 105.0);
                level(ui, "midtonesHigh", tl!("Midtones to:"), 150.0);
            }
        }
    }
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        // Preview: the selection as a grayscale mask, or the image to pick colours from.
        let box_size = vec2(PREVIEW as f32, PREVIEW as f32);
        let (frame, resp) = ui.allocate_exact_size(box_size, Sense::click());
        ui.painter().rect_filled(frame, 0.0, t.canvas);
        let shown = preview(app, ui.ctx(), f);
        if let Some((tex, [w, h], k)) = shown {
            let scale = (box_size.x / w.max(1) as f32).min(box_size.y / h.max(1) as f32);
            let img_rect = Rect::from_center_size(frame.center(), vec2(w as f32 * scale, h as f32 * scale));
            ui.painter().image(tex, img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            // Eyedropper on the preview (Photoshop also samples in the preview area).
            if let Some(p) = resp.interact_pointer_pos().filter(|p| resp.clicked() && c.sampling && img_rect.contains(*p)) {
                let at = [
                    ((p.x - img_rect.left()) / scale).floor().max(0.0) as f64 * f64::from(k) + f64::from(k / 2),
                    ((p.y - img_rect.top()) / scale).floor().max(0.0) as f64 * f64::from(k) + f64::from(k / 2),
                ];
                pick(app, f, at, ui.input(|i| i.modifiers));
            }
        }
        // A preview that can't be drawn says why (#145) instead of staying blank.
        if let Some(e) = app.color_range.as_ref().and_then(|p| p.error.clone()) {
            ui.painter().rect_filled(frame, 0.0, t.canvas);
            let msg = format!(
                "Preview unavailable:
{e}"
            );
            let g = ui.painter().layout(msg, egui::FontId::proportional(11.0), t.text_faint, frame.width() - 16.0);
            ui.painter().galley(frame.center() - g.size() / 2.0, g, t.text_faint);
        }
        ui.painter().rect_stroke(frame, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
        let label = match app.color_range.as_ref().and_then(|p| p.error.as_deref()) {
            Some(e) => format!("Color Range preview unavailable: {e}"),
            None => tl!("Color Range preview").to_string(),
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, ui.is_enabled(), &label));
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.add_enabled_ui(c.sampling, |ui| {
                let tool = s(f, "__tool", "sample").to_string();
                ui.horizontal(|ui| {
                    for (id, badge, label) in
                        [("sample", "", tl!("Eyedropper")), ("add", "+", tl!("Add to Sample")), ("subtract", "−", tl!("Subtract from Sample"))]
                    {
                        if eyedropper(ui, badge, tool == id, label).clicked() {
                            f.insert("__tool".into(), json!(id));
                        }
                    }
                });
                let (add, sub) = (points(f, "points").len(), points(f, "subtractPoints").len());
                let what = match (add, sub) {
                    (0, 0) => tl!("Sample: foreground colour").to_string(),
                    (a, 0) => crate::i18n::trn(crate::i18n::current(), a as u64, "{n} sample", "{n} samples"),
                    (a, s) => crate::i18n::fmt(
                        tl!("{added}, {removed} subtracted"),
                        &[("added", &crate::i18n::trn(crate::i18n::current(), a as u64, "{n} sample", "{n} samples")), ("removed", &s.to_string())],
                    ),
                };
                ui.label(egui::RichText::new(what).size(11.0).color(t.text_faint));
                if add + sub > 0 && widgets::secondary_button(ui, tl!("Clear Samples"), 0.0).clicked() {
                    set_points(f, "points", &[]);
                    set_points(f, "subtractPoints", &[]);
                }
            });
            ui.add_space(8.0);
            let mut inv = flag(f, "invert");
            if widgets::checkbox(ui, &mut inv, tl!("Invert")).changed() {
                f.insert("invert".into(), json!(inv));
            }
        });
    });
    ui.add_space(4.0);
    let view = s(f, "__view", "selection").to_string();
    ui.horizontal(|ui| {
        if radio(ui, view == "selection", "Selection").clicked() {
            f.insert("__view".into(), json!("selection"));
        }
        ui.add_space(8.0);
        if radio(ui, view == "image", "Image").clicked() {
            f.insert("__view".into(), json!("image"));
        }
    });
    if app.session.active().is_none() {
        ui.label(egui::RichText::new(tl!("Open a document to select a colour range.")).color(t.text_faint));
    }
}

/// An eyedropper click at document pixel `at`: the plain eyedropper replaces the samples, the
/// + one (or Shift) adds one, the − one (or Alt) subtracts one.
pub fn pick(app: &PhotocraftApp, f: &mut Map<String, Value>, at: [f64; 2], mods: egui::Modifiers) {
    let Some(st) = app.session.active() else { return };
    let (w, h) = (f64::from(st.doc.size.width.max(1)), f64::from(st.doc.size.height.max(1)));
    let [x, y] = at;
    let at = [x.clamp(0.0, w - 1.0), y.clamp(0.0, h - 1.0)];
    let tool = if mods.shift {
        "add"
    } else if mods.alt {
        "subtract"
    } else {
        s(f, "__tool", "sample")
    };
    match tool {
        "add" => {
            let mut p = points(f, "points");
            p.push(at);
            set_points(f, "points", &p);
        }
        "subtract" => {
            let mut p = points(f, "subtractPoints");
            p.push(at);
            set_points(f, "subtractPoints", &p);
        }
        _ => {
            set_points(f, "points", &[at]);
            set_points(f, "subtractPoints", &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{
        Harness,
        kittest::{NodeT, Queryable},
    };
    use photocraft_doc::{Color, ColorMode, SampleType, Size};
    use photocraft_geom::Rect as GRect;

    /// 40 × 30: red block (0..20, 0..15), blue block (20..40, 0..15), black and white below.
    fn app_with_doc() -> PhotocraftApp {
        let mut doc = Document::with_background("cr", Size::new(40, 30), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let bg = doc.layers[0].surface_mut().unwrap();
        bg.fill_rect(GRect::new(0, 0, 20, 15), &[1.0, 0.0, 0.0, 1.0]);
        bg.fill_rect(GRect::new(20, 0, 40, 15), &[0.0, 0.0, 1.0, 1.0]);
        bg.fill_rect(GRect::new(0, 15, 20, 30), &[0.0, 0.0, 0.0, 1.0]);
        let mut s = photocraft_engine::Session::new();
        s.add_document(doc, None);
        PhotocraftApp::new(s, crate::Services::default())
    }

    fn coverage(app: &PhotocraftApp, x: i32, y: i32) -> f32 {
        app.session.active().unwrap().doc.selection.as_ref().map_or(0.0, |s| s.sample_channel(x, y, 0))
    }

    fn harness(app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui_state(
            |ui, app| {
                crate::menus::menu_bar(app, ui);
                crate::dialogs::show(app, ui.ctx());
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(2);
        h
    }

    fn dialog_id(app: &PhotocraftApp) -> u64 {
        app.ui.dialogs.iter().find(|d| owns(&d.fields)).map(|d| d.id).expect("Color Range dialog open")
    }

    fn set(h: &mut Harness<'static, PhotocraftApp>, key: &str, v: Value) {
        let id = dialog_id(h.state());
        h.state_mut().ui.dialog_mut(id).unwrap().fields.insert(key.into(), v);
        h.run_steps(3);
    }

    /// OK / Cancel are painted buttons without accessibility labels; the dialog's keys do the same.
    fn ok(h: &mut Harness<'static, PhotocraftApp>) {
        h.key_press(egui::Key::Enter);
    }

    fn click(h: &mut Harness<'static, PhotocraftApp>, at: egui::Pos2) {
        h.hover_at(at);
        h.run_steps(1);
        h.drag_at(at);
        h.run_steps(1);
        h.drop_at(at);
    }

    fn disabled(h: &Harness<'static, PhotocraftApp>, label: &str) -> bool {
        h.get_by_label(label).accesskit_node().is_disabled()
    }

    #[test]
    fn opens_from_the_select_menu_without_touching_the_document() {
        let mut h = harness(app_with_doc());
        let rev = h.state().session.active().unwrap().revision;
        h.get_by_label("Select").click();
        h.run_steps(2);
        h.get_by_label("Color Range…").click();
        h.run_steps(3);
        assert!(h.state().ui.dialogs.iter().any(|d| owns(&d.fields)), "Select › Color Range… must open the dialog");
        for label in ["Color Range", "Fuzziness:", "Localized Color Clusters", "Invert", "Selection", "Color Range preview", "Add to Sample"] {
            assert!(h.query_by_label(label).is_some(), "missing {label}");
        }
        assert_eq!(h.query_all_by_label("Image").count(), 2, "the Image menu and the Image preview radio");
        assert_eq!(h.state().session.active().unwrap().revision, rev, "opening must not edit the document");
        // Automation and the generic command dialog get the same dialog.
        let ctx = egui::Context::default();
        let r = crate::menus::invoke(h.state_mut(), &ctx, COMMAND, json!({})).unwrap();
        assert!(r.get("dialog").is_some(), "{r}");
    }

    #[test]
    fn select_mode_switches_the_enabled_controls() {
        let mut h = harness(app_with_doc());
        open(h.state_mut());
        h.run_steps(4);
        // Sampled Colors: Fuzziness, eyedroppers and Localized Color Clusters; Range only when localized.
        assert!(!disabled(&h, "Fuzziness:") && !disabled(&h, "Localized Color Clusters") && !disabled(&h, "Add to Sample"));
        assert!(h.query_by_label("Range:").is_none());
        h.get_by_label("Localized Color Clusters").click();
        h.run_steps(2);
        assert!(h.query_by_label("Range:").is_some(), "Range shows with Localized Color Clusters");
        // A hue family: no Fuzziness, eyedroppers or clusters (Photoshop disables them).
        set(&mut h, "select", json!("reds"));
        assert!(disabled(&h, "Fuzziness:") && disabled(&h, "Localized Color Clusters") && disabled(&h, "Add to Sample"));
        assert!(h.query_by_label("Range:").is_none());
        assert!(!disabled(&h, "Invert"));
        // Tones: Fuzziness and the tonal range.
        set(&mut h, "select", json!("shadows"));
        assert!(!disabled(&h, "Fuzziness:") && disabled(&h, "Localized Color Clusters"));
        assert!(h.query_by_label("Shadows up to:").is_some());
        set(&mut h, "select", json!("midtones"));
        assert!(h.query_by_label("Midtones from:").is_some() && h.query_by_label("Midtones to:").is_some());
        set(&mut h, "select", json!("outOfGamut"));
        assert!(disabled(&h, "Fuzziness:") && h.query_by_label("Midtones from:").is_none());
        assert_eq!(
            controls(&serde_json::from_value(json!({"select": "highlights"})).unwrap()),
            Controls { fuzziness: true, sampling: false, range: false, tonal: true }
        );
    }

    #[test]
    fn dialog_shrinks_back_when_a_mode_needs_fewer_controls() {
        let mut h = harness(app_with_doc());
        open(h.state_mut());
        h.run_steps(4);
        let area = egui::Id::new(("dialog", dialog_id(h.state())));
        let height = |h: &Harness<'static, PhotocraftApp>| h.ctx.memory(|m| m.area_rect(area)).map_or(0.0, |r| r.height());
        set(&mut h, "select", json!("outOfGamut"));
        let short = height(&h);
        set(&mut h, "select", json!("midtones"));
        let tall = height(&h);
        assert!(tall > short + 40.0, "Midtones adds two sliders: {short} → {tall}");
        set(&mut h, "select", json!("outOfGamut"));
        let back = height(&h);
        assert!((back - short).abs() < 1.0, "the dialog must shrink back, not keep an empty band above OK: {short} → {tall} → {back}");
    }

    #[test]
    fn ok_applies_one_undo_step() {
        let mut h = harness(app_with_doc());
        h.state_mut().run("select.rect", json!({"x": 0, "y": 20, "width": 5, "height": 5})).unwrap();
        let past = h.state().session.active().unwrap().history.past_len();
        open(h.state_mut());
        h.run_steps(4);
        set(&mut h, "select", json!("reds"));
        ok(&mut h);
        h.run_steps(2);
        assert!(!h.state().ui.dialogs.iter().any(|d| owns(&d.fields)), "OK closes the dialog");
        let app = h.state();
        assert_eq!((coverage(app, 5, 5), coverage(app, 30, 5), coverage(app, 2, 22)), (1.0, 0.0, 0.0));
        assert_eq!(app.session.active().unwrap().history.past_len(), past + 1, "one history step");
        assert_eq!(app.session.active().unwrap().history.undo_label(), Some("Color Range"));
        h.state_mut().run("edit.undo", json!({})).unwrap();
        let app = h.state();
        assert_eq!((coverage(app, 5, 5), coverage(app, 2, 22)), (0.0, 1.0), "undo restores the previous selection");
    }

    #[test]
    fn cancel_keeps_the_previous_selection() {
        let mut h = harness(app_with_doc());
        h.state_mut().run("select.rect", json!({"x": 0, "y": 20, "width": 5, "height": 5})).unwrap();
        let (rev, past) = {
            let st = h.state().session.active().unwrap();
            (st.revision, st.history.past_len())
        };
        open(h.state_mut());
        h.run_steps(4);
        set(&mut h, "select", json!("blues"));
        h.get_by_label("Invert").click();
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().ui.dialogs.is_empty());
        let app = h.state();
        let st = app.session.active().unwrap();
        assert_eq!((st.revision, st.history.past_len()), (rev, past));
        assert_eq!((coverage(app, 2, 22), coverage(app, 30, 5), coverage(app, 5, 5)), (1.0, 0.0, 0.0));
    }

    #[test]
    fn invert_flips_the_selection() {
        let mut h = harness(app_with_doc());
        open(h.state_mut());
        h.run_steps(4);
        set(&mut h, "select", json!("reds"));
        h.get_by_label("Invert").click();
        h.run_steps(2);
        let id = dialog_id(h.state());
        assert_eq!(h.state_mut().ui.dialog_mut(id).unwrap().fields.get("invert"), Some(&json!(true)));
        ok(&mut h);
        h.run_steps(2);
        let app = h.state();
        assert_eq!((coverage(app, 5, 5), coverage(app, 30, 5), coverage(app, 5, 22), coverage(app, 30, 22)), (0.0, 1.0, 1.0, 1.0));
    }

    #[test]
    fn eyedroppers_pick_on_the_preview() {
        let mut h = harness(app_with_doc());
        open(h.state_mut());
        h.run_steps(4);
        // 40 × 30 at 200 pt: 5 pt per pixel; the image is centred vertically (150 pt tall).
        let at = |h: &Harness<'static, PhotocraftApp>, x: f32, y: f32| {
            h.get_by_label("Color Range preview").rect().left_top() + egui::vec2(x * 5.0 + 2.5, 25.0 + y * 5.0 + 2.5)
        };
        let p = at(&h, 30.0, 5.0);
        click(&mut h, p);
        h.run_steps(2);
        let f = |h: &Harness<'static, PhotocraftApp>| h.state().ui.dialogs.iter().find(|d| owns(&d.fields)).unwrap().fields.clone();
        assert_eq!(points(&f(&h), "points"), vec![[30.0, 5.0]]);
        // Add to Sample: the red block too; the selection then covers both.
        h.get_by_label("Add to Sample").click();
        h.run_steps(1);
        let p = at(&h, 5.0, 5.0);
        click(&mut h, p);
        h.run_steps(2);
        assert_eq!(points(&f(&h), "points").len(), 2);
        // Subtract from Sample: take the blue back out.
        h.get_by_label("Subtract from Sample").click();
        h.run_steps(1);
        let p = at(&h, 35.0, 10.0);
        click(&mut h, p);
        h.run_steps(2);
        assert_eq!(points(&f(&h), "subtractPoints"), vec![[35.0, 10.0]]);
        let p = params(&f(&h));
        assert_eq!(p["select"], "sampledColors");
        ok(&mut h);
        h.run_steps(2);
        let app = h.state();
        assert_eq!((coverage(app, 5, 5), coverage(app, 30, 5), coverage(app, 5, 22)), (1.0, 0.0, 0.0));
    }

    #[test]
    fn params_send_only_what_the_mode_uses() {
        let mut app = app_with_doc();
        let id = open(&mut app);
        let mut f = app.ui.dialog_mut(id).unwrap().fields.clone();
        assert_eq!(params(&f), json!({"select": "sampledColors", "invert": false, "fuzziness": 40.0}));
        // Localized needs a picked point.
        f.insert("localized".into(), json!(true));
        assert!(params(&f).get("localized").is_none());
        pick(&app, &mut f, [3.0, 4.0], egui::Modifiers::NONE);
        pick(&app, &mut f, [99.0, -4.0], egui::Modifiers::SHIFT);
        pick(&app, &mut f, [1.0, 1.0], egui::Modifiers::ALT);
        assert_eq!(
            params(&f),
            json!({"select": "sampledColors", "invert": false, "fuzziness": 40.0, "points": [[3.0, 4.0], [39.0, 0.0]], "subtractPoints": [[1.0, 1.0]], "localized": true, "range": 100.0})
        );
        f.insert("select".into(), json!("midtones"));
        f.insert("midtonesLow".into(), json!(160.0));
        assert_eq!(params(&f), json!({"select": "midtones", "invert": false, "fuzziness": 20.0, "tonalRange": [150.0, 160.0]}));
        f.insert("select".into(), json!("highlights"));
        assert_eq!(params(&f)["tonalRange"], json!(190.0));
        f.insert("select".into(), json!("cyans"));
        assert_eq!(params(&f), json!({"select": "cyans", "invert": false}));
        // Every Select entry is accepted by the engine.
        for (v, _) in SELECTS {
            f.insert("select".into(), json!(v));
            app.run(COMMAND, params(&f)).unwrap();
        }
    }

    /// #145: the whole app without a GPU (no wgpu render state: the CPU canvas, as on a Linux
    /// machine whose adapter can't run the GPU canvas), on a small window. Select › Color Range…
    /// from the menu opens the dialog on screen and draws both previews.
    #[test]
    fn opens_and_previews_in_the_full_app_without_a_gpu() {
        let mut h = Harness::builder().with_size(egui::vec2(1024.0, 600.0)).build_eframe(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            assert!(cc.wgpu_render_state.is_none(), "this test covers the no-GPU path");
            app_with_doc()
        });
        h.run_steps(6);
        assert!(h.state().gpu.is_none(), "the CPU canvas");
        h.get_all_by_label("Select").next().expect("the Select menu").click();
        h.run_steps(3);
        h.get_by_label("Color Range…").click();
        h.run_steps(6);
        let id = dialog_id(h.state());
        let area = h.ctx.memory(|m| m.area_rect(egui::Id::new(("dialog", id)))).expect("the dialog is drawn");
        assert!(h.ctx.content_rect().contains_rect(area), "the dialog is on screen: {area:?}");
        assert!(h.query_by_label("Color Range preview").is_some());
        let p = h.state().color_range.as_ref().expect("the preview is cached");
        assert!(p.mask.is_some() && p.error.is_none(), "the selection preview is drawn: {:?}", p.error);
        set(&mut h, "__view", json!("image"));
        let p = h.state().color_range.as_ref().unwrap();
        assert!(p.image.is_some() && p.error.is_none(), "the image preview is drawn: {:?}", p.error);
        set(&mut h, "select", json!("reds"));
        ok(&mut h);
        h.run_steps(3);
        assert_eq!(coverage(h.state(), 5, 5), 1.0);
    }

    #[test]
    fn a_preview_failure_is_shown_not_silent() {
        let mut h = harness(app_with_doc());
        open(h.state_mut());
        h.run_steps(3);
        // The engine refuses these params: the preview says so instead of staying blank.
        let id = dialog_id(h.state());
        h.state_mut().ui.dialog_mut(id).unwrap().fields.insert("select".into(), json!("noSuchMode"));
        h.run_steps(3);
        let err = h.state().color_range.as_ref().and_then(|p| p.error.clone());
        assert!(err.is_some(), "the failure is recorded");
        assert!(h.query_by_label_contains("Color Range preview unavailable").is_some(), "and shown");
        set(&mut h, "select", json!("reds"));
        assert!(h.state().color_range.as_ref().unwrap().error.is_none(), "and cleared once the preview works again");
    }
}
