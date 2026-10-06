//! File › Print…, Print One Copy, File › Package… and File › Export › Paths to Illustrator….
//!
//! Printing renders the flattened document into a PDF page (paper size, orientation, position,
//! scale, colour handling with printer profile / intent / black-point compensation, corner and
//! centre crop marks, registration marks, description and label) with a small PDF writer
//! (ISO 32000: one page, a Flate-compressed image XObject, vector marks, Helvetica text), then
//! hands it to the system spooler with `lp` (CUPS, macOS and Linux). `"dryRun": true` stops
//! before spooling and reports the command line, which is how the tests exercise the path.
//!
//! Paths to Illustrator writes the document's paths as an Adobe Illustrator 3 compatible
//! PostScript file (`m`/`L`/`C` path construction, `n`/`N` unpainted closed/open paths, `*u`/`*U`
//! compound paths), following the public PostScript Language Reference and the AI3 format notes.

use std::io::Write as _;

use photocraft_color::ColorMode;
use photocraft_doc::{Document, LayerContent, SmartSource};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::file_cmds::{file_name, join, native_doc, read_file, stem, write_file};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

// ---------- PDF ----------

/// Paper sizes in points (portrait).
pub const PAPERS: [(&str, f64, f64); 8] = [
    ("letter", 612.0, 792.0),
    ("legal", 612.0, 1008.0),
    ("tabloid", 792.0, 1224.0),
    ("a3", 841.89, 1190.55),
    ("a4", 595.28, 841.89),
    ("a5", 419.53, 595.28),
    ("4x6", 288.0, 432.0),
    ("5x7", 360.0, 504.0),
];

/// The image placed on a print page.
pub struct PrintImage {
    pub width: u32,
    pub height: u32,
    /// 1 (gray), 3 (RGB) or 4 (CMYK, 1 = full ink), 8-bit interleaved.
    pub channels: usize,
    pub data: Vec<u8>,
    pub icc: Option<Vec<u8>>,
}

/// Printer marks around the image.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Marks {
    pub corner_crop: bool,
    pub center_crop: bool,
    pub registration: bool,
}

/// One page: paper (pt), image box (pt, origin bottom-left), marks and texts.
pub struct PrintPage {
    pub paper: (f64, f64),
    pub image: PrintImage,
    pub rect: (f64, f64, f64, f64),
    pub marks: Marks,
    pub description: Option<String>,
    pub label: Option<String>,
}

fn pdf_string(s: &str) -> String {
    let mut o = String::from("(");
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                o.push('\\');
                o.push(c);
            }
            c if c.is_ascii() && !c.is_ascii_control() => o.push(c),
            _ => o.push('?'),
        }
    }
    o.push(')');
    o
}

fn deflate(data: &[u8]) -> Vec<u8> {
    // Fast: print PDFs are transient and large; level 1 is ~4× quicker than the default.
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// Vector marks around `(x, y, w, h)`.
fn marks_ops(m: Marks, (x, y, w, h): (f64, f64, f64, f64)) -> String {
    let mut s = String::from("q 0 G 0.3 w\n");
    let (gap, len) = (6.0, 18.0);
    let line = |s: &mut String, x0: f64, y0: f64, x1: f64, y1: f64| s.push_str(&format!("{x0:.2} {y0:.2} m {x1:.2} {y1:.2} l S\n"));
    if m.corner_crop {
        for (cx, cy, dx, dy) in [(x, y, -1.0, -1.0), (x + w, y, 1.0, -1.0), (x, y + h, -1.0, 1.0), (x + w, y + h, 1.0, 1.0)] {
            line(&mut s, cx + dx * gap, cy, cx + dx * (gap + len), cy);
            line(&mut s, cx, cy + dy * gap, cx, cy + dy * (gap + len));
        }
    }
    if m.center_crop {
        let (mx, my) = (x + w / 2.0, y + h / 2.0);
        line(&mut s, mx, y - gap, mx, y - gap - len);
        line(&mut s, mx, y + h + gap, mx, y + h + gap + len);
        line(&mut s, x - gap, my, x - gap - len, my);
        line(&mut s, x + w + gap, my, x + w + gap + len, my);
    }
    if m.registration {
        let r = 5.0;
        let off = gap + len / 2.0;
        for (cx, cy) in [(x + w / 2.0, y - off), (x + w / 2.0, y + h + off), (x - off, y + h / 2.0), (x + w + off, y + h / 2.0)] {
            // Circle (four Béziers) plus crosshair.
            let k = 0.5523 * r;
            s.push_str(&format!(
                "{:.2} {cy:.2} m {:.2} {:.2} {:.2} {:.2} {cx:.2} {:.2} c {:.2} {:.2} {:.2} {:.2} {:.2} {cy:.2} c {:.2} {:.2} {:.2} {:.2} {cx:.2} {:.2} c {:.2} {:.2} {:.2} {:.2} {:.2} {cy:.2} c S\n",
                cx + r, cx + r, cy + k, cx + k, cy + r, cy + r, cx - k, cy + r, cx - r, cy + k, cx - r, cx - r, cy - k, cx - k, cy - r, cy - r, cx + k, cy - r, cx + r, cy - k, cx + r
            ));
            line(&mut s, cx - r * 1.6, cy, cx + r * 1.6, cy);
            line(&mut s, cx, cy - r * 1.6, cx, cy + r * 1.6);
        }
    }
    s.push_str("Q\n");
    s
}

/// A one-page PDF.
pub fn print_pdf(page: &PrintPage) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    let img = &page.image;
    let (pw, ph) = page.paper;
    let (x, y, w, h) = page.rect;
    // 1 catalog, 2 pages, 3 page, 4 contents, 5 image, 6 font, 7 ICC (optional).
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut out, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    obj(
        &mut out,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Resources << /XObject << /Im0 5 0 R >> /Font << /F1 6 0 R >> >> /Contents 4 0 R >>"
        )
        .as_bytes(),
    );
    // An explicit white page, so every viewer (and rasteriser) shows paper, not transparency.
    let mut content = format!("q 1 g 0 0 {pw:.2} {ph:.2} re f Q\nq {w:.3} 0 0 {h:.3} {x:.3} {y:.3} cm /Im0 Do Q\n");
    content.push_str(&marks_ops(page.marks, page.rect));
    let any_marks = page.marks.corner_crop || page.marks.center_crop || page.marks.registration;
    let pad = if any_marks { 30.0 } else { 6.0 };
    if let Some(l) = page.label.as_deref().filter(|l| !l.is_empty()) {
        content.push_str(&format!("BT /F1 8 Tf 0 g {:.2} {:.2} Td {} Tj ET\n", x, y + h + pad, pdf_string(l)));
    }
    if let Some(d) = page.description.as_deref().filter(|d| !d.is_empty()) {
        content.push_str(&format!("BT /F1 8 Tf 0 g {:.2} {:.2} Td {} Tj ET\n", x, y - pad - 8.0, pdf_string(d)));
    }
    let c = deflate(content.as_bytes());
    let mut body = format!("<< /Length {} /Filter /FlateDecode >>\nstream\n", c.len()).into_bytes();
    body.extend_from_slice(&c);
    body.extend_from_slice(b"\nendstream");
    obj(&mut out, &body);
    let device = match img.channels {
        1 => "/DeviceGray",
        4 => "/DeviceCMYK",
        _ => "/DeviceRGB",
    };
    let cs = if img.icc.is_some() { "[/ICCBased 7 0 R]".to_string() } else { device.to_string() };
    let z = deflate(&img.data);
    let mut body = format!(
        "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {cs} /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
        img.width,
        img.height,
        z.len()
    )
    .into_bytes();
    body.extend_from_slice(&z);
    body.extend_from_slice(b"\nendstream");
    obj(&mut out, &body);
    obj(&mut out, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    if let Some(icc) = &img.icc {
        let z = deflate(icc);
        let mut body = format!("<< /N {} /Alternate {device} /Filter /FlateDecode /Length {} >>\nstream\n", img.channels, z.len()).into_bytes();
        body.extend_from_slice(&z);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &body);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1).as_bytes());
    out
}

// ---------- Print ----------

/// The flattened, colour-handled document as 8-bit samples in its print colour space.
fn print_image(doc: &Document, p: &Value, cmd: &str) -> Result<(PrintImage, Value)> {
    let mut t = Session::new();
    t.add_document(doc.clone(), None);
    let handling = p.get("colorHandling").and_then(Value::as_str).unwrap_or("printerManages");
    let mut info = json!({"colorHandling": handling});
    if !matches!(doc.mode, ColorMode::Rgb | ColorMode::Grayscale | ColorMode::Cmyk) {
        t.execute("image.mode.rgb", json!({}))?;
    }
    match handling {
        "photocraftManages" | "photoshopManages" => {
            let profile = p.get("printerProfile").and_then(Value::as_str).ok_or_else(|| bad(cmd, "\"photocraftManages\" needs a \"printerProfile\""))?;
            let intent = p.get("intent").and_then(Value::as_str).unwrap_or("relative");
            let bpc = p.get("bpc").and_then(Value::as_bool).unwrap_or(true);
            t.execute("edit.convertToProfile", json!({"profile": profile, "intent": intent, "bpc": bpc}))?;
            info["printerProfile"] = json!(profile);
            info["intent"] = json!(intent);
            info["bpc"] = json!(bpc);
        }
        "printerManages" | "noColorManagement" => {}
        "separations" => return Err(bad(cmd, "Separations printing is not supported; use photocraftManages with a CMYK printer profile")),
        h => return Err(bad(cmd, format!("unknown colorHandling `{h}` (printerManages|photocraftManages|noColorManagement)"))),
    }
    let d = &t.active().ok_or(EngineError::NoDocument)?.doc;
    let icc = if handling == "noColorManagement" { None } else { d.icc_profile.as_ref().map(|v| v.to_vec()) };
    // RGB and Grayscale: the composite over white is the print image (no layer flatten).
    if matches!(d.mode, ColorMode::Rgb | ColorMode::Grayscale) {
        let gray = d.mode == ColorMode::Grayscale;
        let buf = photocraft_compose::flatten(d).over_background([1.0, 1.0, 1.0]);
        let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let data: Vec<u8> =
            if gray { buf.px.iter().map(|p| to8(p[0])).collect() } else { buf.px.iter().flat_map(|p| [to8(p[0]), to8(p[1]), to8(p[2])]).collect() };
        return Ok((PrintImage { width: d.size.width, height: d.size.height, channels: if gray { 1 } else { 3 }, data, icc }, info));
    }
    t.execute("layer.flattenImage", json!({}))?;
    let d = &t.active().ok_or(EngineError::NoDocument)?.doc;
    let fmt = d.pixel_format();
    let k = fmt.mode.color_channels();
    let n = fmt.channels();
    let surf = d.layers.first().and_then(|l| l.surface()).ok_or_else(|| other("nothing to print"))?;
    let vals = surf.read_region(d.bounds());
    let mut data = Vec::with_capacity(vals.len() / n * k);
    for px in vals.chunks_exact(n) {
        for v in &px[..k] {
            data.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    Ok((PrintImage { width: d.size.width, height: d.size.height, channels: k, data, icc }, info))
}

fn paper(p: &Value, cmd: &str) -> Result<(f64, f64)> {
    let (w, h) = match p.get("paper") {
        None => (612.0, 792.0),
        Some(Value::String(name)) => PAPERS
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, w, h)| (*w, *h))
            .ok_or_else(|| bad(cmd, format!("unknown paper `{name}` ({})", PAPERS.map(|p| p.0).join("|"))))?,
        Some(Value::Array(a)) if a.len() == 2 => (a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)),
        _ => return Err(bad(cmd, "paper is a name or [width, height] in points")),
    };
    if !(w > 36.0 && h > 36.0) {
        return Err(bad(cmd, "paper is too small"));
    }
    Ok(if p.get("orientation").and_then(Value::as_str) == Some("landscape") { (h.max(w), w.min(h)) } else { (w, h) })
}

/// Where the image lands on the paper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrintLayout {
    /// Paper size in points (after orientation).
    pub paper: (f64, f64),
    /// Image box in points, origin bottom-left.
    pub rect: (f64, f64, f64, f64),
    /// Print scale (1.0 = 100 %).
    pub scale: f64,
    pub marks: Marks,
}

/// The print layout for `doc` with Print parameters `p` (used by the dialog's preview too).
pub fn layout(doc: &Document, p: &Value, cmd: &str) -> Result<PrintLayout> {
    let (pw, ph) = paper(p, cmd)?;
    let dpi = f64::from(doc.resolution_dpi.max(1.0));
    let (iw, ih) = (f64::from(doc.size.width) * 72.0 / dpi, f64::from(doc.size.height) * 72.0 / dpi);
    let marks = Marks {
        corner_crop: p.get("cornerCropMarks").and_then(Value::as_bool).unwrap_or(false),
        center_crop: p.get("centerCropMarks").and_then(Value::as_bool).unwrap_or(false),
        registration: p.get("registrationMarks").and_then(Value::as_bool).unwrap_or(false),
    };
    let margin = if marks.corner_crop || marks.center_crop || marks.registration { 36.0 } else { 18.0 };
    let fit = p.get("scaleToFit").and_then(Value::as_bool).unwrap_or(false);
    let scale = if fit {
        ((pw - 2.0 * margin) / iw).min((ph - 2.0 * margin) / ih)
    } else {
        p.get("scale").and_then(Value::as_f64).filter(|v| *v > 0.0).unwrap_or(100.0) / 100.0
    };
    let (w, h) = (iw * scale, ih * scale);
    let center = p.get("center").and_then(Value::as_bool).unwrap_or(true);
    let (x, y) = if center {
        ((pw - w) / 2.0, (ph - h) / 2.0)
    } else {
        // Top / left in inches from the paper's top-left corner (Photoshop's Position fields).
        let top = p.get("top").and_then(Value::as_f64).unwrap_or(0.0) * 72.0;
        let left = p.get("left").and_then(Value::as_f64).unwrap_or(0.0) * 72.0;
        (left, ph - top - h)
    };
    Ok(PrintLayout { paper: (pw, ph), rect: (x, y, w, h), scale, marks })
}

/// Lays out, renders and (unless dry-run) spools a print. Shared by Print and Print One Copy.
fn do_print(s: &mut Session, p: &Value, cmd: &str) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let PrintLayout { paper: (pw, ph), rect: (x, y, w, h), scale, marks } = layout(&doc, p, cmd)?;
    let (img, color) = print_image(&doc, p, cmd)?;
    let description = p
        .get("description")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| crate::file_cmds::read_file_info(doc.metadata.xmp.as_deref())["description"].as_str().unwrap_or_default().to_string());
    let label = p.get("labels").and_then(Value::as_bool).unwrap_or(false).then(|| doc.name.clone());
    let page = PrintPage { paper: (pw, ph), image: img, rect: (x, y, w, h), marks, description, label };
    let pdf = print_pdf(&page);
    let output = p.get("output").and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
    let send = p.get("send").and_then(Value::as_bool).unwrap_or(output.is_none());
    let pdf_path = match &output {
        Some(o) => o.clone(),
        None => {
            // Unique per call: concurrent prints of same-named documents must not share a spool file.
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tmp = std::env::temp_dir().to_string_lossy().into_owned();
            join(&tmp, &format!("{}-print-{}-{n}.pdf", crate::file_cmds::sanitize(&stem(&doc.name)), std::process::id()))
        }
    };
    write_file(&pdf_path, &pdf)?;
    let copies = crate::commands::int(p, "copies").unwrap_or(1).clamp(1, 999);
    let mut argv: Vec<String> = vec!["lp".into()];
    if let Some(pr) = p.get("printer").and_then(Value::as_str).filter(|v| !v.is_empty()) {
        argv.extend(["-d".into(), pr.to_string()]);
    }
    if copies > 1 {
        argv.extend(["-n".into(), copies.to_string()]);
    }
    argv.extend(["-t".into(), doc.name.clone(), pdf_path.clone()]);
    let dry = p.get("dryRun").and_then(Value::as_bool).unwrap_or(false);
    let mut sent = false;
    let mut spool = Value::Null;
    if send && !dry {
        spool = json!(spool_pdf(&argv)?);
        sent = true;
    }
    let mut remembered = p.clone();
    if let Some(o) = remembered.as_object_mut() {
        o.remove("output");
        o.remove("dryRun");
    }
    s.file_menu.last_print = Some(remembered);
    crate::automate_cmds::fire_event(s, "print");
    Ok(
        json!({"pdf": pdf_path, "bytes": pdf.len(), "paper": [pw, ph], "imageRect": [x, y, w, h], "scale": scale * 100.0, "copies": copies, "command": if send { json!(argv) } else { Value::Null }, "sent": sent, "spooler": spool, "color": color}),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn spool_pdf(argv: &[String]) -> Result<String> {
    let out = std::process::Command::new(&argv[0]).args(&argv[1..]).output().map_err(|e| other(format!("could not run `lp` (CUPS): {e}")))?;
    if !out.status.success() {
        return Err(other(format!("lp failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
#[cfg(target_arch = "wasm32")]
fn spool_pdf(_: &[String]) -> Result<String> {
    Err(other("printing needs the desktop app"))
}

fn print(s: &mut Session, p: &Value) -> Result<Value> {
    do_print(s, p, "file.print")
}

fn print_one_copy(s: &mut Session, p: &Value) -> Result<Value> {
    let mut q = s.file_menu.last_print.clone().unwrap_or_else(|| json!({}));
    if let (Some(o), Some(extra)) = (q.as_object_mut(), p.as_object()) {
        for (k, v) in extra {
            o.insert(k.clone(), v.clone());
        }
    }
    q["copies"] = json!(1);
    do_print(s, &q, "file.printOneCopy")
}

// ---------- Package ----------

fn package(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.package";
    let dir = p.get("dir").and_then(Value::as_str).filter(|v| !v.is_empty()).ok_or_else(|| bad(cmd, "missing \"dir\" (where to create the package folder)"))?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = (*d.doc).clone();
    let name = stem(&d.path.clone().unwrap_or_else(|| doc.name.clone()));
    // The native format keeps external links; our PSD writer has no `lnkE` (external linked
    // file) support yet, so a PSD package would embed or flatten them.
    let ext = match p.get("format").and_then(Value::as_str) {
        Some(f @ ("psd" | "psb" | "pcraft")) => f.to_string(),
        Some(f) => return Err(bad(cmd, format!("unknown package format `{f}` (pcraft|psd|psb)"))),
        None => "pcraft".into(),
    };
    let folder = join(dir, &crate::file_cmds::sanitize(&name));
    let links_dir = join(&folder, "Links");
    // Copy every linked file once; relink the copy of the document to the copies.
    let mut copied: Vec<(String, String)> = Vec::new();
    let mut relinks: Vec<(photocraft_doc::LayerId, String)> = Vec::new();
    let mut missing = Vec::new();
    for (_, _, l) in doc.walk() {
        let LayerContent::Smart(so) = &l.content else { continue };
        let SmartSource::Linked { path } = &so.source else { continue };
        let target = match copied.iter().find(|(src, _)| src == path) {
            Some((_, t)) => t.clone(),
            None => {
                let mut fname = file_name(path);
                if copied.iter().any(|(_, t)| file_name(t) == fname) {
                    fname = format!("{}-{}", copied.len(), fname);
                }
                let t = join(&links_dir, &fname);
                match read_file(path) {
                    Ok(bytes) => {
                        write_file(&t, &bytes)?;
                        copied.push((path.clone(), t.clone()));
                        t
                    }
                    Err(_) => {
                        missing.push(path.clone());
                        continue;
                    }
                }
            }
        };
        relinks.push((l.id, target));
    }
    let mut t = Session::new();
    t.add_document(doc, None);
    for (id, path) in &relinks {
        t.execute("layer.smartObjects.relinkToFile", json!({"layer": id.0, "path": path}))?;
    }
    let out = join(&folder, &format!("{}.{ext}", crate::file_cmds::sanitize(&name)));
    let pdoc = t.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let warnings = crate::file_cmds::save_doc(&pdoc, &out, None)?;
    Ok(json!({"folder": folder, "document": out, "links": copied.iter().map(|(_, t)| t).collect::<Vec<_>>(), "missing": missing, "warnings": warnings}))
}

// ---------- Paths to Illustrator ----------

/// The document's paths as an Illustrator 3 PostScript file. `which`: "all", "work" or a path
/// name. Coordinates are points (pixels at the document resolution), y up.
pub fn paths_to_ai(doc: &Document, which: &str) -> Result<(String, usize)> {
    let dpi = f64::from(doc.resolution_dpi.max(1.0));
    let k = 72.0 / dpi;
    let (w, h) = (f64::from(doc.size.width) * k, f64::from(doc.size.height) * k);
    let mut chosen: Vec<(String, &photocraft_doc::Path)> = Vec::new();
    match which {
        "all" => {
            chosen.extend(doc.paths.iter().map(|p| (p.name.clone(), &p.path)));
            if let Some(wp) = &doc.work_path {
                chosen.push(("Work Path".into(), wp));
            }
        }
        "work" => chosen.extend(doc.work_path.iter().map(|wp| ("Work Path".to_string(), wp))),
        name => chosen.extend(doc.paths.iter().filter(|p| p.name == name).map(|p| (p.name.clone(), &p.path))),
    }
    let mut s = String::new();
    s.push_str("%!PS-Adobe-2.0 EPSF-1.2\n%%Creator: PhotoCraft\n");
    s.push_str(&format!("%%Title: ({})\n", doc.name.replace([')', '('], "_")));
    s.push_str(&format!(
        "%%BoundingBox: 0 0 {} {}\n%%HiResBoundingBox: 0 0 {w:.4} {h:.4}\n%AI3_Cropmarks: 0 0 {w:.4} {h:.4}\n",
        w.ceil() as i64,
        h.ceil() as i64
    ));
    s.push_str("%%DocumentProcessColors: Black\n%%EndComments\n%%EndProlog\n%%BeginSetup\n%%EndSetup\n");
    let pt = |x: f64, y: f64| (x * k, h - y * k);
    let mut n = 0;
    for (name, path) in &chosen {
        let subs: Vec<_> = path.subpaths.iter().filter(|sp| !sp.knots.is_empty()).collect();
        if subs.is_empty() {
            continue;
        }
        n += 1;
        s.push_str(&format!("%%Note: {name}\n"));
        let compound = subs.len() > 1;
        if compound {
            s.push_str("*u\n");
        }
        for sp in subs {
            let (x, y) = pt(sp.knots[0].anchor.x, sp.knots[0].anchor.y);
            s.push_str(&format!("{x:.4} {y:.4} m\n"));
            for seg in sp.segments() {
                let [a, c1, c2, b] = seg;
                let straight = c1 == a && c2 == b;
                let (bx, by) = pt(b.x, b.y);
                if straight {
                    s.push_str(&format!("{bx:.4} {by:.4} L\n"));
                } else {
                    let (x1, y1) = pt(c1.x, c1.y);
                    let (x2, y2) = pt(c2.x, c2.y);
                    s.push_str(&format!("{x1:.4} {y1:.4} {x2:.4} {y2:.4} {bx:.4} {by:.4} C\n"));
                }
            }
            // Unpainted: `n` closes the path, `N` leaves it open.
            s.push_str(if sp.closed { "n\n" } else { "N\n" });
        }
        if compound {
            s.push_str("*U\n");
        }
    }
    s.push_str("%%PageTrailer\n%%Trailer\n%%EOF\n");
    Ok((s, n))
}

fn paths_to_illustrator(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.export.pathsToIllustrator";
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let which = p.get("paths").and_then(Value::as_str).unwrap_or("all");
    let (text, n) = paths_to_ai(&d.doc, which)?;
    if n == 0 {
        return Err(bad(cmd, format!("no paths match `{which}`")));
    }
    match p.get("path").and_then(Value::as_str).filter(|v| !v.is_empty()) {
        Some(path) => {
            write_file(path, text.as_bytes())?;
            Ok(json!({"path": path, "paths": n}))
        }
        None => Ok(json!({"ai": text, "paths": n})),
    }
}

fn has_paths(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    if d.doc.paths.is_empty() && d.doc.work_path.is_none() { Err("the document has no paths".into()) } else { Ok(()) }
}

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $sc:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: $sc, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    const PRINT_PARAMS: &str = r##"{"printer":name? (default printer),"copies":1..999=1,"paper":"letter|legal|tabloid|a3|a4|a5|4x6|5x7"|[w,h] pt="letter","orientation":"portrait|landscape"="portrait","center":bool=true,"top":in?,"left":in?,"scale":%=100,"scaleToFit":bool=false,"colorHandling":"printerManages|photocraftManages|noColorManagement"="printerManages","printerProfile":profile? (photocraftManages),"intent":"perceptual|relative|saturation|absolute"="relative","bpc":bool=true,"cornerCropMarks":bool,"centerCropMarks":bool,"registrationMarks":bool,"description":bool,"labels":bool,"output":pdf path? (print to PDF; then "send" defaults to false),"send":bool?,"dryRun":bool=false (render the PDF, report the lp command, don't spool)} → {pdf, imageRect, command, sent}"##;
    vec![
        spec!("file.print", "Print…", &["File"], Some("Cmd+P"), PRINT_PARAMS, native_doc, print),
        spec!(
            "file.printOneCopy",
            "Print One Copy",
            &["File"],
            Some("Cmd+Alt+Shift+P"),
            "{} (the last Print settings, one copy; any Print key overrides)",
            native_doc,
            print_one_copy
        ),
        spec!(
            "file.package",
            "Package…",
            &["File"],
            None,
            r##"{"dir":folder,"format":"pcraft|psd|psb"="pcraft"} → {folder, document, links, missing} (copies the document and its linked files into <dir>/<name>/, relinked to Links/)"##,
            native_doc,
            package
        ),
        spec!(
            "file.export.pathsToIllustrator",
            "Paths to Illustrator…",
            &["File", "Export"],
            None,
            r##"{"path":str? (.ai; omit to return the text),"paths":"all|work|<path name>"="all"} → {path, paths}"##,
            has_paths,
            paths_to_illustrator
        ),
    ]
}

#[cfg(test)]
#[path = "print_cmds/tests.rs"]
mod tests;
