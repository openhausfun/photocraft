//! Front ends of the File-menu commands added with slices: the Save for Web (Legacy) dialog
//! (Original / Optimized / 2-Up / 4-Up previews, GIF / PNG-8 / PNG-24 / JPEG / WBMP settings,
//! image size, per-slice output), the Print dialog (page preview with position, scale, marks;
//! colour handling), and form dialogs for Contact Sheet II, Create Droplet, Statistics, Script
//! Events Manager, Package and Paths to Illustrator. Every OK runs the engine command with the
//! dialog's fields, so the control channel can drive them with `ui.dialog.set` / `confirm`.

use std::sync::Arc;

use photocraft_engine::web_cmds::{self, Optimized, WebSettings};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;
use crate::theme::Tokens;

fn no_params(p: &Value) -> bool {
    p.as_object().is_none_or(|o| o.is_empty())
}

fn default_dir(app: &PhotocraftApp) -> String {
    app.session.active().and_then(|d| d.path.as_deref()).and_then(|p| p.rfind(['/', '\\']).map(|i| p[..i].to_string())).unwrap_or_else(|| ".".into())
}

fn doc_stem(app: &PhotocraftApp) -> String {
    app.session.active().map(|d| d.doc.name.rsplit_once('.').map_or(d.doc.name.clone(), |(a, _)| a.to_string())).unwrap_or_else(|| tl!("Untitled").into())
}

/// A generic form dialog for `command` (rendered by `view_cmds::form_body`).
fn form(app: &mut PhotocraftApp, command: &str, label: &str, fields: Value, choices: Value) -> Value {
    let mut f = Map::new();
    f.insert("__command".into(), json!(command));
    f.insert("__label".into(), json!(label));
    f.insert("__form".into(), json!(true));
    f.insert("__choices".into(), choices);
    if let Value::Object(m) = fields {
        f.extend(m);
    }
    json!({"dialog": app.ui.open_dialog(DialogKind::Command, f)})
}

/// Menu items fronted here (dialogs before the engine command). `None` when not ours.
pub fn invoke(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !no_params(params) {
        return None;
    }
    let dir = default_dir(app);
    let stem = doc_stem(app);
    Some(Ok(match id {
        "file.export.saveForWebLegacy" => {
            app.session.active()?;
            json!({"dialog": open_web(app)})
        }
        "file.print" => {
            app.session.active()?;
            json!({"dialog": open_print(app)})
        }
        "file.printOneCopy" if app.session.file_menu.last_print.is_none() => {
            app.session.active()?;
            json!({"dialog": open_print(app)})
        }
        "file.export.exportPreferences" => return crate::prefs_ui::invoke(app, ctx, "edit.preferences.export", &json!({})),
        "file.automate.contactSheetII" => form(
            app,
            id,
            tl!("Contact Sheet II"),
            json!({"input": dir, "units": "inches", "width": 8.0, "height": 10.0, "resolution": 300.0, "mode": "rgb", "depth": 8, "columns": 5, "rows": 6, "placeAcrossFirst": true, "autoSpacing": true, "rotateForBestFit": false, "caption": true, "font": photocraft_text::fonts::DEFAULT_FAMILY, "fontSize": 12.0, "flatten": false}),
            json!({"units": ["inches", "cm", "pixels"], "mode": ["rgb", "gray", "cmyk", "lab"]}),
        ),
        "file.automate.createDroplet" => {
            let a = &app.ui.actions;
            let action = a.selected.and_then(|i| a.list.get(i)).or(a.list.first());
            let Some(action) = action else {
                return Some(Err("record an action in the Actions panel first".into()));
            };
            let steps: Vec<Value> = action.steps.iter().map(|(id, p)| json!([id, p])).collect();
            let name = action.name.clone();
            form(
                app,
                id,
                tl!("Create Droplet"),
                json!({"path": format!("{dir}/{name}.pcdroplet"), "name": name, "steps": steps, "output": format!("{dir}/droplet-output"), "format": "same"}),
                json!({"format": ["same", "png", "jpg", "psd", "tiff"]}),
            )
        }
        "file.scripts.statistics" => {
            let modes: Vec<&str> = photocraft_doc::StackMode::ALL.iter().map(|m| m.id()).collect();
            form(app, id, "Image Statistics", json!({"mode": "median", "input": dir, "align": false}), json!({"mode": modes}))
        }
        "file.scripts.browse" => {
            let (name, bytes) = app.services.pick_open.as_mut().and_then(|f| f())?;
            let r = app.run(id, json!({"script": String::from_utf8_lossy(&bytes)}));
            if r.is_ok() {
                app.ui.status = format!("Ran script {name}");
            }
            return Some(r);
        }
        "file.scripts.scriptEventsManager" => {
            let enabled = app.session.prefs().script_events.enabled;
            let events: Vec<&str> = photocraft_engine::automate_cmds::EVENTS.iter().map(|e| e.0).collect();
            form(app, id, "Script Events Manager", json!({"enabled": enabled, "event": "openDocument", "script": "", "name": ""}), json!({"event": events}))
        }
        "file.package" => form(app, id, "Package", json!({"dir": dir}), json!({})),
        "file.export.pathsToIllustrator" => {
            let mut names = vec!["all".to_string()];
            if let Some(d) = app.session.active() {
                if d.doc.work_path.is_some() {
                    names.push("work".into());
                }
                names.extend(d.doc.paths.iter().map(|p| p.name.clone()));
            }
            form(app, id, "Export Paths to File", json!({"path": format!("{dir}/{stem}.ai"), "paths": "all"}), json!({"paths": names}))
        }
        _ => return None,
    }))
}

/// Checked state of toggle items owned here.
pub fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    match id {
        "view.lockSlices" => Some(app.session.file_menu.slices_locked),
        "file.generate.imageAssets" => Some(app.session.active().is_some_and(|d| app.session.file_menu.image_assets.contains(&d.doc.id))),
        _ => None,
    }
}

/// Dialogs whose body and confirm live here.
pub fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key("__web") || f.contains_key("__print")
}

pub fn dialog_width(f: &Map<String, Value>) -> Option<f32> {
    if f.contains_key("__web") {
        Some(900.0)
    } else if f.contains_key("__print") {
        Some(720.0)
    } else {
        None
    }
}

pub fn ok_label(f: &Map<String, Value>) -> Option<&'static str> {
    if f.contains_key("__web") {
        Some(tl!("Save…"))
    } else if f.contains_key("__print") {
        Some(tl!("Print"))
    } else {
        None
    }
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    if f.contains_key("__web") {
        web_body(app, ui, f);
    } else {
        print_body(app, ui, f);
    }
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    if f.contains_key("__web") { web_confirm(app, f) } else { print_confirm(app, f) }
}

fn params(f: &Map<String, Value>) -> Value {
    Value::Object(f.iter().filter(|(k, _)| !k.starts_with("__")).map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn s(f: &Map<String, Value>, k: &str, d: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or(d).to_string()
}
fn n(f: &Map<String, Value>, k: &str, d: f64) -> f64 {
    f.get(k).and_then(Value::as_f64).unwrap_or(d)
}
fn b(f: &Map<String, Value>, k: &str, d: bool) -> bool {
    f.get(k).and_then(Value::as_bool).unwrap_or(d)
}

// ---------- Save for Web ----------

const WEB_FORMATS: [(&str, &str); 5] = [("gif", "GIF"), ("png8", "PNG-8"), ("png24", "PNG-24"), ("jpeg", "JPEG"), ("wbmp", "WBMP")];

pub fn open_web(app: &mut PhotocraftApp) -> u64 {
    let mut f = Map::new();
    f.insert("__web".into(), json!(true));
    f.insert("__label".into(), json!("Save for Web (Legacy)"));
    f.insert("__view".into(), json!("2up"));
    // Start from the last settings, like Photoshop.
    let last = app.session.file_menu.last_web.clone().unwrap_or_else(|| json!({"format": "jpeg", "quality": 60}));
    if let Value::Object(m) = last {
        for (k, v) in m {
            if !matches!(k.as_str(), "path" | "dir") {
                f.insert(k, v);
            }
        }
    }
    for (k, v) in [
        ("format", json!("jpeg")),
        ("quality", json!(60)),
        ("palette", json!("selective")),
        ("colors", json!(128)),
        ("dither", json!("diffusion")),
        ("ditherAmount", json!(88)),
        ("transparency", json!(true)),
        ("matte", json!("#ffffff")),
        ("interlaced", json!(false)),
        ("progressive", json!(false)),
        ("optimized", json!(true)),
        ("embedIcc", json!(false)),
        ("convertToSrgb", json!(true)),
        ("metadata", json!("copyright")),
        ("percent", json!(100.0)),
        ("webSnap", json!(0)),
    ] {
        f.entry(k.to_string()).or_insert(v);
    }
    app.ui.open_dialog(DialogKind::Command, f)
}

/// Settings variants shown in 4-Up (current, then two smaller alternatives).
fn variants(p: &Value) -> Vec<Value> {
    let mut out = vec![p.clone()];
    let fmt = p["format"].as_str().unwrap_or("jpeg");
    for k in [2.0, 4.0] {
        let mut v = p.clone();
        if fmt == "jpeg" {
            v["quality"] = json!((p["quality"].as_f64().unwrap_or(60.0) / k).round().max(1.0));
        } else if fmt == "gif" || fmt == "png8" {
            v["colors"] = json!((p["colors"].as_f64().unwrap_or(128.0) / k).round().max(2.0));
        } else {
            v["format"] = json!(if k == 2.0 { "png8" } else { "gif" });
            v["colors"] = json!(256 / k as u32);
        }
        out.push(v);
    }
    out
}

/// The flattened preview image: pixels, width, height, full-size / proxy pixel ratio.
type WebProxy = (Arc<Vec<[f32; 4]>>, u32, u32, f64);
type WebCache = (String, Arc<egui::TextureHandle>, Option<(usize, Option<usize>, &'static str)>);

/// Renders one preview pane: the original (`None`) or an optimised variant.
fn web_pane(app: &PhotocraftApp, ui: &mut egui::Ui, size: egui::Vec2, p: Option<&Value>, full_px: f64) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.clone();
    let rev = st.revision;
    // The proxy: the export size, at most 640 px on the long side.
    let base = p.cloned().unwrap_or_else(|| json!({}));
    let size_sig = format!("{}|{}|{}", base["percent"], base["width"], base["height"]);
    let wkey = egui::Id::new(("web-proxy", doc.id.0, rev, size_sig.clone()));
    let proxy: Option<WebProxy> = ui.data(|d| d.get_temp(wkey));
    let (px, w, h, ratio) = match proxy {
        Some(v) => v,
        None => {
            let pct = base["percent"].as_f64().unwrap_or(100.0).max(1.0) / 100.0;
            let (fw, fh) = (f64::from(doc.size.width) * pct, f64::from(doc.size.height) * pct);
            let k = (640.0 / fw.max(fh)).min(1.0);
            let st = WebSettings::default();
            let Ok((wd, _, _)) =
                web_cmds::web_document(&doc, &json!({"width": (fw * k).round().max(1.0), "height": (fh * k).round().max(1.0), "resample": "bilinear"}), &st)
            else {
                return;
            };
            let buf = photocraft_compose::flatten(&wd);
            let v = (Arc::new(buf.px), wd.size.width, wd.size.height, (fw * fh) / (f64::from(wd.size.width) * f64::from(wd.size.height)).max(1.0));
            ui.data_mut(|d| d.insert_temp(wkey, v.clone()));
            v
        }
    };
    let sig = format!("{}{}", size_sig, p.map(|v| v.to_string()).unwrap_or_else(|| "original".into()));
    let key = egui::Id::new(("web-pane", doc.id.0, rev, sig.clone()));
    let cached: Option<WebCache> = ui.data(|d| d.get_temp(key));
    let (tex, info) = match cached.filter(|c| c.0 == sig) {
        Some((_, tex, info)) => (tex, info),
        None => {
            let (rgba, info) = match p {
                None => (px.iter().flat_map(|q| q.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect::<Vec<u8>>(), None),
                Some(p) => match WebSettings::from_params(p, "preview")
                    .and_then(|st| web_cmds::optimize(&px, w as usize, photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &st, None, None, 72.0, true))
                {
                    Ok(Optimized { preview, bytes, colors, ext, .. }) => (preview, Some(((bytes.len() as f64 * ratio) as usize, colors, ext))),
                    Err(_) => (vec![0; (w * h * 4) as usize], None),
                },
            };
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            let tex = Arc::new(ui.ctx().load_texture("web-pane", img, egui::TextureOptions::LINEAR));
            ui.data_mut(|d| d.insert_temp(key, (sig, tex.clone(), info)));
            (tex, info)
        }
    };
    ui.vertical(|ui| {
        let (r, _) = ui.allocate_exact_size(size - egui::vec2(0.0, 34.0), egui::Sense::hover());
        crate::widgets::checker(ui.painter(), r, 8.0);
        let ts = tex.size_vec2();
        let k = (r.width() / ts.x).min(r.height() / ts.y).min(1.0);
        let ir = egui::Rect::from_center_size(r.center(), ts * k);
        ui.painter().image(tex.id(), ir, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Outside);
        let _ = full_px;
        let (l1, l2) = match (p, info) {
            (None, _) => ("Original".to_string(), format!("{}  ·  {} × {} px", doc.name, doc.size.width, doc.size.height)),
            (Some(_), Some((bytes, colors, ext))) => {
                // Photoshop's download estimate at 56.6 Kbps.
                let secs = (bytes as f64 * 8.0 / 56_600.0).ceil();
                (
                    format!("{}  {}", ext.to_uppercase(), crate::sizing::human_bytes(bytes as f64)),
                    format!("{secs} sec @ 56.6 Kbps{}", colors.map(|c| format!("  ·  {c} colors")).unwrap_or_default()),
                )
            }
            (Some(_), None) => ("—".into(), String::new()),
        };
        ui.label(egui::RichText::new(l1).color(t.text).size(11.5));
        ui.label(egui::RichText::new(l2).color(t.text_dim).size(11.0));
    });
}

fn dropdown_str(ui: &mut egui::Ui, id: &str, f: &mut Map<String, Value>, key: &str, opts: &[(&str, &str)], w: f32) {
    let mut v = s(f, key, opts.first().map_or("", |o| o.0));
    let o: Vec<(String, &str)> = opts.iter().map(|(k, l)| (k.to_string(), *l)).collect();
    if crate::widgets::dropdown(ui, id, &mut v, &o, w) {
        f.insert(key.into(), json!(v));
    }
}

fn check(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, d: bool) {
    let mut v = b(f, key, d);
    crate::widgets::checkbox(ui, &mut v, label);
    f.insert(key.into(), json!(v));
}

fn number(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, range: std::ops::RangeInclusive<f32>, unit: &str, d: f64) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim));
        let mut v = n(f, key, d) as f32;
        crate::widgets::value_field(ui, &mut v, range, unit, 64.0);
        f.insert(key.into(), json!(v.round()));
    });
}

fn web_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let Some(doc) = app.session.active().map(|d| d.doc.clone()) else { return };
    ui.horizontal(|ui| {
        let mut view = s(f, "__view", "2up");
        for (k, l) in [("original", tl!("Original")), ("optimized", tl!("Optimized")), ("2up", "2-Up"), ("4up", "4-Up")] {
            if ui.selectable_label(view == k, tl!(&l)).clicked() {
                view = k.into();
            }
        }
        f.insert("__view".into(), json!(view));
    });
    ui.add_space(4.0);
    let p = params(f);
    let full_px = f64::from(doc.size.width) * f64::from(doc.size.height);
    ui.horizontal_top(|ui| {
        // Previews.
        let area = egui::vec2(600.0, 470.0);
        ui.vertical(|ui| {
            ui.set_width(area.x);
            match s(f, "__view", "2up").as_str() {
                "original" => web_pane(app, ui, area, None, full_px),
                "optimized" => web_pane(app, ui, area, Some(&p), full_px),
                "2up" => {
                    ui.horizontal(|ui| {
                        web_pane(app, ui, egui::vec2(area.x / 2.0 - 4.0, area.y), None, full_px);
                        web_pane(app, ui, egui::vec2(area.x / 2.0 - 4.0, area.y), Some(&p), full_px);
                    });
                }
                _ => {
                    let v = variants(&p);
                    let cell = egui::vec2(area.x / 2.0 - 4.0, area.y / 2.0 - 2.0);
                    ui.horizontal(|ui| {
                        web_pane(app, ui, cell, None, full_px);
                        web_pane(app, ui, cell, Some(&v[0]), full_px);
                    });
                    ui.horizontal(|ui| {
                        web_pane(app, ui, cell, Some(&v[1]), full_px);
                        web_pane(app, ui, cell, Some(&v[2]), full_px);
                    });
                }
            }
        });
        ui.add_space(10.0);
        // Settings.
        ui.vertical(|ui| {
            ui.set_width(250.0);
            ui.label(egui::RichText::new(tl!("Preset")).color(t.text_dim));
            let mut preset = s(f, "__preset", "");
            let mut opts: Vec<(String, &str)> = vec![("".into(), "[Unnamed]")];
            opts.extend(web_cmds::PRESETS.iter().map(|p| (p.to_string(), *p)));
            if crate::widgets::dropdown(ui, "web-preset", &mut preset, &opts, 230.0) && !preset.is_empty() {
                if let Some(Value::Object(m)) = web_cmds::preset_settings(&preset).map(|st| st.to_params()) {
                    f.extend(m);
                }
                f.insert("__preset".into(), json!(preset));
            }
            ui.add_space(4.0);
            dropdown_str(ui, "web-format", f, "format", &WEB_FORMATS, 230.0);
            ui.add_space(4.0);
            let fmt = s(f, "format", "jpeg");
            match fmt.as_str() {
                "gif" | "png8" => {
                    dropdown_str(
                        ui,
                        "web-palette",
                        f,
                        "palette",
                        &[
                            ("perceptual", tl!("Perceptual")),
                            ("selective", tl!("Selective")),
                            ("adaptive", tl!("Adaptive")),
                            ("restrictive", tl!("Restrictive (Web)")),
                            ("exact", tl!("Exact")),
                        ],
                        150.0,
                    );
                    number(ui, f, "colors", tl!("Colors"), 2.0..=256.0, "", 128.0);
                    dropdown_str(
                        ui,
                        "web-dither",
                        f,
                        "dither",
                        &[("none", tl!("No Dither")), ("diffusion", tl!("Diffusion")), ("pattern", tl!("Pattern")), ("noise", tl!("Noise"))],
                        150.0,
                    );
                    number(ui, f, "ditherAmount", tl!("Dither"), 0.0..=100.0, "%", 88.0);
                    check(ui, f, "transparency", tl!("Transparency"), true);
                    check(ui, f, "interlaced", tl!("Interlaced"), false);
                    number(ui, f, "webSnap", tl!("Web Snap"), 0.0..=100.0, "%", 0.0);
                }
                "jpeg" => {
                    number(ui, f, "quality", tl!("Quality"), 0.0..=100.0, "", 60.0);
                    check(ui, f, "progressive", tl!("Progressive"), false);
                    check(ui, f, "optimized", tl!("Optimized"), true);
                    check(ui, f, "embedIcc", tl!("Embed Color Profile"), false);
                }
                "wbmp" => {
                    dropdown_str(
                        ui,
                        "web-dither",
                        f,
                        "dither",
                        &[("none", tl!("No Dither")), ("diffusion", tl!("Diffusion")), ("pattern", tl!("Pattern"))],
                        150.0,
                    );
                }
                _ => {
                    check(ui, f, "transparency", tl!("Transparency"), true);
                    check(ui, f, "interlaced", tl!("Interlaced"), false);
                }
            }
            if fmt != "png24" || !b(f, "transparency", true) {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl!("Matte")).color(t.text_dim));
                    let mut m = s(f, "matte", "#ffffff");
                    if ui.add(egui::TextEdit::singleline(&mut m).desired_width(80.0)).changed() {
                        f.insert("matte".into(), json!(m));
                    }
                });
            }
            ui.add_space(6.0);
            check(ui, f, "convertToSrgb", tl!("Convert to sRGB"), true);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Metadata")).color(t.text_dim));
                dropdown_str(
                    ui,
                    "web-meta",
                    f,
                    "metadata",
                    &[("none", tl!("None")), ("copyright", tl!("Copyright")), ("copyrightAndContact", tl!("Copyright and Contact Info")), ("all", tl!("All"))],
                    160.0,
                );
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Image Size")).font(crate::theme::semibold(12.0)).color(t.text));
            number(ui, f, "percent", tl!("Percent"), 1.0..=1000.0, "%", 100.0);
            let pct = n(f, "percent", 100.0) / 100.0;
            ui.label(
                egui::RichText::new(format!("W: {} px   H: {} px", (f64::from(doc.size.width) * pct).round(), (f64::from(doc.size.height) * pct).round()))
                    .color(t.text_dim)
                    .size(11.5),
            );
            let nslices = photocraft_doc::slices::resolve(&doc).len();
            if !doc.slices.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(format!("{nslices} slices")).color(t.text_dim));
                check(ui, f, "html", tl!("Save HTML and Images"), true);
                dropdown_str(ui, "web-slices", f, "slices", &[("all", tl!("All Slices")), ("user", tl!("All User Slices"))], 150.0);
            }
        });
    });
}

fn web_confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let mut p = params(f);
    let doc = app.session.active().ok_or("no document")?.doc.clone();
    let has_slices = !doc.slices.is_empty();
    if p.get("path").is_none() && p.get("dir").is_none() {
        let st = WebSettings::from_params(&p, "file.export.saveForWebLegacy").map_err(|e| e.to_string())?;
        let base = photocraft_doc::slices::base_name(&doc);
        let suggested = if has_slices && b(f, "html", true) { format!("{base}.html") } else { format!("{base}.{}", st.format.ext()) };
        let path = app.services.pick_save.as_mut().and_then(|pick| pick(&suggested)).ok_or("cancelled")?;
        if has_slices {
            let dir = path.rfind(['/', '\\']).map(|i| path[..i].to_string()).unwrap_or_else(|| ".".into());
            p["dir"] = json!(dir);
            p["html"] = json!(b(f, "html", true));
        } else {
            p["path"] = json!(path);
        }
    }
    let r = app.run("file.export.saveForWebLegacy", p)?;
    app.ui.status = format!("Saved for Web: {} file(s)", r["files"].as_array().map_or(0, Vec::len));
    Ok(r)
}

// ---------- Print ----------

pub fn open_print(app: &mut PhotocraftApp) -> u64 {
    let mut f = Map::new();
    f.insert("__print".into(), json!(true));
    f.insert("__label".into(), json!("PhotoCraft Print Settings"));
    if let Some(Value::Object(m)) = app.session.file_menu.last_print.clone() {
        f.extend(m);
    }
    for (k, v) in [
        ("printer", json!("")),
        ("copies", json!(1)),
        ("paper", json!("letter")),
        ("orientation", json!("portrait")),
        ("colorHandling", json!("printerManages")),
        ("printerProfile", json!("coated-cmyk")),
        ("intent", json!("relative")),
        ("bpc", json!(true)),
        ("center", json!(true)),
        ("top", json!(0.0)),
        ("left", json!(0.0)),
        ("scale", json!(100.0)),
        ("scaleToFit", json!(false)),
        ("cornerCropMarks", json!(false)),
        ("centerCropMarks", json!(false)),
        ("registrationMarks", json!(false)),
        ("description", json!(false)),
        ("labels", json!(false)),
        ("output", json!("")),
    ] {
        f.entry(k.to_string()).or_insert(v);
    }
    app.ui.open_dialog(DialogKind::Command, f)
}

fn print_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let Some(doc) = app.session.active().map(|d| d.doc.clone()) else { return };
    let p = params(f);
    ui.horizontal_top(|ui| {
        // Page preview.
        ui.vertical(|ui| {
            let (area, _) = ui.allocate_exact_size(egui::vec2(330.0, 400.0), egui::Sense::hover());
            ui.painter().rect_filled(area, 0.0, t.canvas);
            match photocraft_engine::print_cmds::layout(&doc, &p, "file.print") {
                Ok(l) => {
                    let (pw, ph) = l.paper;
                    let k = ((area.width() - 20.0) / pw as f32).min((area.height() - 20.0) / ph as f32);
                    let page = egui::Rect::from_center_size(area.center(), egui::vec2(pw as f32 * k, ph as f32 * k));
                    ui.painter().rect_filled(page, 0.0, egui::Color32::WHITE);
                    ui.painter().rect_stroke(page, 0.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Outside);
                    let (x, y, w, h) = l.rect;
                    let img = egui::Rect::from_min_size(
                        egui::pos2(page.left() + x as f32 * k, page.bottom() - (y + h) as f32 * k),
                        egui::vec2(w as f32 * k, h as f32 * k),
                    );
                    let key = egui::Id::new(("print-thumb", doc.id.0, app.session.active().map_or(0, |d| d.revision)));
                    let tex: Option<Arc<egui::TextureHandle>> = ui.data(|d| d.get_temp(key));
                    let tex = tex.unwrap_or_else(|| {
                        let th = photocraft_compose::thumbnail(&doc, 400);
                        let ci = egui::ColorImage::from_rgba_unmultiplied([th.width as usize, th.height as usize], &th.pixels);
                        let tx = Arc::new(ui.ctx().load_texture("print-thumb", ci, egui::TextureOptions::LINEAR));
                        ui.data_mut(|d| d.insert_temp(key, tx.clone()));
                        tx
                    });
                    let clip = ui.painter().with_clip_rect(page);
                    clip.image(tex.id(), img, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    let mark = egui::Stroke::new(1.0, egui::Color32::BLACK);
                    if l.marks.corner_crop {
                        for (c, dx, dy) in
                            [(img.left_top(), -1.0, -1.0), (img.right_top(), 1.0, -1.0), (img.left_bottom(), -1.0, 1.0), (img.right_bottom(), 1.0, 1.0)]
                        {
                            clip.line_segment([c + egui::vec2(dx * 3.0, 0.0), c + egui::vec2(dx * 12.0, 0.0)], mark);
                            clip.line_segment([c + egui::vec2(0.0, dy * 3.0), c + egui::vec2(0.0, dy * 12.0)], mark);
                        }
                    }
                    if l.marks.registration {
                        for c in [
                            img.center_top() - egui::vec2(0.0, 9.0),
                            img.center_bottom() + egui::vec2(0.0, 9.0),
                            img.left_center() - egui::vec2(9.0, 0.0),
                            img.right_center() + egui::vec2(9.0, 0.0),
                        ] {
                            clip.circle_stroke(c, 3.5, mark);
                        }
                    }
                    let fits = x >= 0.0 && y >= 0.0 && x + w <= pw && y + h <= ph;
                    let note = if fits { String::new() } else { "  ·  larger than the paper".to_string() };
                    ui.label(
                        egui::RichText::new(format!("Scale {:.1}%  ·  {:.2} × {:.2} in{note}", l.scale * 100.0, w / 72.0, h / 72.0))
                            .color(if fits { t.text_dim } else { t.warning })
                            .size(11.0),
                    );
                }
                Err(e) => {
                    ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, e.to_string(), egui::FontId::proportional(12.0), t.text_dim);
                }
            }
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.set_width(340.0);
            let head = |ui: &mut egui::Ui, s: &str| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(s).font(crate::theme::semibold(12.0)).color(t.text));
            };
            head(ui, tl!("Printer Setup"));
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Printer")).color(t.text_dim));
                let mut pr = s(f, "printer", "");
                if ui.add(egui::TextEdit::singleline(&mut pr).hint_text(tl!("Default printer")).desired_width(180.0)).changed() {
                    f.insert("printer".into(), json!(pr));
                }
            });
            number(ui, f, "copies", tl!("Copies"), 1.0..=999.0, "", 1.0);
            ui.horizontal(|ui| {
                let papers: Vec<(&str, &str)> = photocraft_engine::print_cmds::PAPERS.iter().map(|p| (p.0, p.0)).collect();
                dropdown_str(ui, "print-paper", f, "paper", &papers, 110.0);
                dropdown_str(ui, "print-orient", f, "orientation", &[("portrait", tl!("Portrait")), ("landscape", tl!("Landscape"))], 110.0);
            });
            head(ui, tl!("Color Management"));
            dropdown_str(
                ui,
                "print-color",
                f,
                "colorHandling",
                &[
                    ("printerManages", tl!("Printer Manages Colors")),
                    ("photocraftManages", tl!("PhotoCraft Manages Colors")),
                    ("noColorManagement", tl!("No Color Management")),
                ],
                240.0,
            );
            if s(f, "colorHandling", "") == "photocraftManages" {
                dropdown_str(
                    ui,
                    "print-profile",
                    f,
                    "printerProfile",
                    &[
                        ("coated-cmyk", tl!("Coated CMYK")),
                        ("srgb", "sRGB IEC61966-2.1"),
                        ("adobe-rgb-compat", tl!("Adobe RGB (1998) compatible")),
                        ("display-p3", tl!("Display P3")),
                        ("gray-gamma-2.2", tl!("Gray Gamma 2.2")),
                    ],
                    240.0,
                );
                ui.horizontal(|ui| {
                    dropdown_str(
                        ui,
                        "print-intent",
                        f,
                        "intent",
                        &[
                            ("perceptual", tl!("Perceptual")),
                            ("relative", tl!("Relative Colorimetric")),
                            ("saturation", tl!("Saturation")),
                            ("absolute", tl!("Absolute Colorimetric")),
                        ],
                        170.0,
                    );
                    check(ui, f, "bpc", tl!("Black Point Compensation"), true);
                });
            }
            head(ui, tl!("Position and Size"));
            check(ui, f, "center", tl!("Center"), true);
            if !b(f, "center", true) {
                ui.horizontal(|ui| {
                    number(ui, f, "top", tl!("Top"), 0.0..=100.0, "in", 0.0);
                    number(ui, f, "left", tl!("Left"), 0.0..=100.0, "in", 0.0);
                });
            }
            check(ui, f, "scaleToFit", tl!("Scale to Fit Media"), false);
            if !b(f, "scaleToFit", false) {
                number(ui, f, "scale", tl!("Scale"), 1.0..=1000.0, "%", 100.0);
            }
            head(ui, tl!("Printing Marks"));
            ui.horizontal(|ui| {
                check(ui, f, "cornerCropMarks", tl!("Corner Crop Marks"), false);
                check(ui, f, "centerCropMarks", tl!("Center Crop Marks"), false);
            });
            ui.horizontal(|ui| {
                check(ui, f, "registrationMarks", tl!("Registration Marks"), false);
                check(ui, f, "description", tl!("Description"), false);
                check(ui, f, "labels", tl!("Labels"), false);
            });
            head(ui, tl!("Save as PDF"));
            let mut out = s(f, "output", "");
            if ui.add(egui::TextEdit::singleline(&mut out).hint_text(tl!("(print to the printer)")).desired_width(300.0)).changed() {
                f.insert("output".into(), json!(out));
            }
        });
    });
}

fn print_confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let r = app.run("file.print", params(f))?;
    app.ui.status = if r["sent"] == json!(true) {
        format!("Sent to the printer ({})", r["spooler"].as_str().unwrap_or(""))
    } else {
        format!("Printed to {}", r["pdf"].as_str().unwrap_or(""))
    };
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (PhotocraftApp, egui::Context) {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 120, "height": 80, "name": "web"})).unwrap();
        (app, egui::Context::default())
    }

    #[test]
    fn save_for_web_dialog_runs_the_command() {
        let (mut app, ctx) = app();
        let r = crate::menus::invoke(&mut app, &ctx, "file.export.saveForWebLegacy", json!({})).unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let dir = std::env::temp_dir().join(format!("pc-webui-{}", std::process::id()));
        let path = dir.join("web.gif").to_string_lossy().into_owned();
        let d = app.ui.dialog_mut(id).unwrap();
        d.fields.insert("format".into(), json!("gif"));
        d.fields.insert("path".into(), json!(path));
        let r = crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(r["files"][0], json!(path));
        assert!(std::fs::read(&path).unwrap().starts_with(b"GIF89a"));
        // The next dialog starts from those settings.
        let r = crate::menus::invoke(&mut app, &ctx, "file.export.saveForWebLegacy", json!({})).unwrap();
        let d = app.ui.dialog_mut(r["dialog"].as_u64().unwrap()).unwrap();
        assert_eq!(d.fields["format"], "gif");
        assert!(!d.fields.contains_key("path"));
    }

    #[test]
    fn print_dialog_and_toggles() {
        let (mut app, ctx) = app();
        let r = crate::menus::invoke(&mut app, &ctx, "file.print", json!({})).unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let out = std::env::temp_dir().join(format!("pc-printui-{}.pdf", std::process::id())).to_string_lossy().into_owned();
        app.ui.dialog_mut(id).unwrap().fields.insert("output".into(), json!(out));
        let r = crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(r["sent"], false);
        assert!(std::fs::read(&out).unwrap().starts_with(b"%PDF"));
        assert_eq!(checked(&app, "view.lockSlices"), Some(false));
        crate::menus::invoke(&mut app, &ctx, "view.lockSlices", json!({})).unwrap();
        assert_eq!(checked(&app, "view.lockSlices"), Some(true));
        crate::menus::invoke(&mut app, &ctx, "file.generate.imageAssets", json!({})).unwrap();
        assert_eq!(checked(&app, "file.generate.imageAssets"), Some(true));
        // Form dialogs open for the rest.
        for id in ["file.automate.contactSheetII", "file.scripts.statistics", "file.scripts.scriptEventsManager", "file.package"] {
            let r = crate::menus::invoke(&mut app, &ctx, id, json!({})).unwrap();
            assert!(r["dialog"].is_u64(), "{id}");
        }
    }
}
