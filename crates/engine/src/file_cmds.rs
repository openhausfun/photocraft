//! File-menu commands beyond New/Close: Close All / Others, Revert, Save a Copy, Open As, Place
//! Embedded / Linked, File Info, the Automate and Scripts items (Fit Image, Conditional Mode
//! Change, Batch, Image Processor, Load Files into Stack, Flatten All Layer Effects / Masks),
//! Export › Layers to Files and Color Lookup Tables, plus the document-level guide layouts of the
//! View menu (New Guide Layout, New Guides From Shape, Clear Canvas Guides).
//!
//! Commands that touch the file system take explicit paths (the UI asks for them with its own
//! pickers) and are unavailable on the web, where there is no file system; their byte-level
//! cores ([`place_bytes`], [`open_bytes_as`]) work everywhere.

use std::sync::Arc;

use photocraft_algo::resample::{Resample, resize_surface, translate_surface};
use photocraft_color::{ColorMode, PixelFormat};
use photocraft_doc::{Affine, Document, Layer, LayerContent, LayerId, SmartObject, SmartSource};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, layer_param};
use crate::{EngineError, Result, Session};

// ---------- predicates ----------

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

/// The file system is reachable (not on the web).
pub(crate) fn native(_: &Session) -> std::result::Result<(), String> {
    if cfg!(target_arch = "wasm32") { Err("not available on the web (no file system)".into()) } else { Ok(()) }
}

pub(crate) fn native_doc(s: &Session) -> std::result::Result<(), String> {
    native(s)?;
    has_doc(s)
}

fn can_revert(s: &Session) -> std::result::Result<(), String> {
    native(s)?;
    let d = s.active().ok_or("no document open")?;
    let path = d.path.as_deref().ok_or("the document has never been saved")?;
    if !is_file(path) {
        return Err(format!("{path} is not a file on disk"));
    }
    Ok(())
}

fn has_adjustments(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    if d.doc.layers.iter().any(|l| l.visible && matches!(l.content, LayerContent::Adjustment(_))) {
        Ok(())
    } else {
        Err("the document has no visible adjustment layers to export".into())
    }
}

fn has_shape(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    match d.active_layer.and_then(|id| d.doc.layer(id)).map(|l| &l.content) {
        Some(LayerContent::Shape(_)) => Ok(()),
        _ => Err("the active layer is not a shape layer".into()),
    }
}

// ---------- file system (native only) ----------

#[cfg(not(target_arch = "wasm32"))]
fn is_file(path: &str) -> bool {
    std::path::Path::new(path).is_file()
}
#[cfg(target_arch = "wasm32")]
fn is_file(_path: &str) -> bool {
    false
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_file(path: &str) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))
}
#[cfg(target_arch = "wasm32")]
pub(crate) fn read_file(path: &str) -> Result<Vec<u8>> {
    Err(EngineError::Other(format!("cannot read {path}: no file system on the web")))
}

#[cfg(not(target_arch = "wasm32"))]
/// Every file the engine writes goes through here: crash-safe (temp file + fsync + rename, see
/// [`photocraft_format::atomic`]), so a failed save never destroys the previous file.
pub(crate) fn write_file(path: &str, bytes: &[u8]) -> Result<()> {
    photocraft_format::atomic_write(std::path::Path::new(path), bytes).map_err(|e| EngineError::Other(e.to_string()))
}
#[cfg(target_arch = "wasm32")]
pub(crate) fn write_file(path: &str, _bytes: &[u8]) -> Result<()> {
    Err(EngineError::Other(format!("cannot write {path}: no file system on the web")))
}

/// Files of a folder that look like images we can open, sorted by name.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn list_images(dir: &str) -> Result<Vec<String>> {
    let rd = std::fs::read_dir(dir).map_err(|e| EngineError::Other(format!("{dir}: {e}")))?;
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()).is_some_and(|x| OPENABLE.contains(&x.to_ascii_lowercase().as_str())))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    out.sort();
    Ok(out)
}
#[cfg(target_arch = "wasm32")]
pub(crate) fn list_images(dir: &str) -> Result<Vec<String>> {
    Err(EngineError::Other(format!("cannot list {dir}: no file system on the web")))
}

/// Extensions the batch commands pick up from a folder.
const OPENABLE: &[&str] = &[
    "psd", "psb", "pcraft", "png", "jpg", "jpeg", "tif", "tiff", "webp", "gif", "bmp", "tga", "exr", "hdr", "qoi", "ico", "pnm", "ppm", "pgm", "dng", "cr2",
    "nef", "nrw", "arw", "pef",
];

pub(crate) fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

pub(crate) fn stem(path: &str) -> String {
    let n = file_name(path);
    match n.rfind('.') {
        Some(i) if i > 0 => n[..i].to_string(),
        _ => n,
    }
}

pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with('/') || dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// A file-name-safe version of a layer name.
pub(crate) fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "layer".into() } else { s }
}

pub(crate) fn import(name: &str, bytes: &[u8]) -> Result<Document> {
    photocraft_io::import(name, bytes).map(|r| r.document).map_err(|e| EngineError::Other(format!("{name}: {e}")))
}

/// Encodes `doc` for `path`'s extension. `quality` is Photoshop's 0–12 JPEG scale.
pub(crate) fn encode(doc: &Document, path: &str, quality: Option<f64>) -> Result<(Vec<u8>, Vec<String>)> {
    let mut opts = photocraft_io::ExportOptions::default();
    if let Some(q) = quality {
        opts.encode.jpeg_quality = (q.clamp(0.0, 12.0) / 12.0 * 99.0 + 1.0).round() as u8;
    }
    photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| EngineError::Other(format!("{path}: {e}")))
}

pub(crate) fn save_doc(doc: &Document, path: &str, quality: Option<f64>) -> Result<Vec<String>> {
    let (bytes, warnings) = encode(doc, path, quality)?;
    write_file(path, &bytes)?;
    Ok(warnings)
}

pub(crate) fn str_param<'a>(p: &'a Value, key: &str, cmd: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|v| !v.is_empty()).ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: format!("missing \"{key}\"") })
}

pub(crate) fn f64_param(p: &Value, key: &str) -> Option<f64> {
    p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite())
}

/// A size parameter; zero or negative means "not set" (dialogs send 0 for empty fields).
fn size_param(p: &Value, key: &str) -> Option<f64> {
    f64_param(p, key).filter(|v| *v > 0.0)
}

// ---------- pixels ----------

/// A composite buffer as a surface in `fmt` (straight RGBA → the format's model and depth).
pub(crate) fn buffer_surface(buf: &photocraft_compose::Buffer, fmt: PixelFormat) -> Surface {
    let n = fmt.channels();
    let mut data = vec![0.0f32; buf.px.len() * n];
    for (p, out) in buf.px.iter().zip(data.chunks_exact_mut(n)) {
        from_rgba_into(&fmt, *p, out);
    }
    let mut s = Surface::new(fmt);
    if !buf.rect.is_empty() {
        s.write_region(buf.rect, &data);
    }
    s.prune();
    s
}

/// The flattened image of `doc` as a surface in `fmt`, placed at the origin.
pub(crate) fn flattened(doc: &Document, fmt: PixelFormat) -> Surface {
    photocraft_compose::flatten_to_surface(doc, fmt, None)
}

// ---------- close / revert / save a copy / open as ----------

fn close_all(s: &mut Session) -> Result<Value> {
    let n = s.documents().len();
    while !s.documents().is_empty() {
        s.close(s.documents().len() - 1);
    }
    Ok(json!({"closed": n}))
}

fn close_others(s: &mut Session, p: &Value) -> Result<Value> {
    let keep = p.get("document").and_then(Value::as_u64).map(|v| v as usize).or(s.active_index()).ok_or(EngineError::NoDocument)?;
    if keep >= s.documents().len() {
        return Err(EngineError::BadParams { cmd: "file.closeOthers".into(), msg: format!("no document {keep}") });
    }
    let n = s.documents().len() - 1;
    for i in (0..s.documents().len()).rev() {
        if i != keep {
            s.close(i);
        }
    }
    s.set_active(0);
    Ok(json!({"closed": n}))
}

/// File › Revert: reload the saved file as one history step (undoable, like Photoshop's
/// "Revert" history state); the document is clean afterwards.
fn revert(s: &mut Session) -> Result<Value> {
    let path = s.active().and_then(|d| d.path.clone()).ok_or(EngineError::Other("the document has never been saved".into()))?;
    let bytes = read_file(&path)?;
    let fresh = import(&file_name(&path), &bytes)?;
    s.edit("Revert", |doc, active| {
        let (id, name) = (doc.id, doc.name.clone());
        *doc = fresh;
        // Keep the identity (colour state, views and caches are keyed by it) and the tab name.
        doc.id = id;
        doc.name = name;
        *active = doc.top_layer();
        Ok(())
    })?;
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    st.saved_revision = st.revision;
    Ok(json!({"path": path, "layers": st.doc.layers.len()}))
}

fn save_a_copy(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_param(p, "path", "file.saveACopy")?.to_string();
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let mut doc = (*d.doc).clone();
    if p.get("layers").and_then(Value::as_bool) == Some(false) {
        // "Layers" unchecked in Save a Copy: write the flattened image.
        let fmt = doc.pixel_format();
        let px = flattened(&doc, fmt);
        doc.layers = vec![Layer::new("Background", LayerContent::Raster(px))];
    }
    let warnings = save_doc(&doc, &path, f64_param(p, "quality"))?;
    Ok(json!({"path": path, "warnings": warnings}))
}

/// Open a file's bytes, decoding them as the format `as_ext` (Open As) when given.
pub fn open_bytes_as(s: &mut Session, name: &str, bytes: &[u8], as_ext: Option<&str>, path: Option<String>) -> Result<Value> {
    let decode_name = match as_ext {
        Some(ext) => format!("{}.{}", stem(name), ext.trim_start_matches('.')),
        None => name.to_string(),
    };
    let r = photocraft_io::import(&decode_name, bytes).map_err(|e| EngineError::Other(format!("{decode_name}: {e}")))?;
    let mut doc = r.document;
    doc.name = file_name(name);
    // Color Settings policies (preserve / convert / discard the embedded profile).
    let (i, color) = s.open_document(doc, path);
    // Import notes (e.g. how a camera raw was developed, or that only its preview opened).
    Ok(json!({"document": i, "color": color, "warnings": r.warnings}))
}

fn open_as(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_param(p, "path", "file.openAs")?.to_string();
    let bytes = read_file(&path)?;
    let as_ext = p.get("as").or_else(|| p.get("format")).and_then(Value::as_str);
    open_bytes_as(s, &path, &bytes, as_ext, Some(path.clone()))
}

// ---------- place ----------

/// Place a file's bytes as a smart object layer, centred and (when larger than the canvas)
/// scaled down to fit, like Photoshop's Place with "Resize Image During Place". `linked` makes it
/// a linked smart object that refers to that path instead of embedding the bytes.
pub fn place_bytes(s: &mut Session, name: &str, bytes: Vec<u8>, linked: Option<String>, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (cw, ch) = (d.doc.size.width as f64, d.doc.size.height as f64);
    let fmt = d.doc.pixel_format();
    let src = import(name, &bytes)?;
    let (w, h) = (src.size.width as f64, src.size.height as f64);
    let scale = match f64_param(p, "scale") {
        Some(k) => (k / 100.0).max(1e-4),
        None if p.get("fit").and_then(Value::as_bool) != Some(false) && (w > cw || h > ch) => (cw / w).min(ch / h),
        None => 1.0,
    };
    let center = match p.get("center").and_then(Value::as_array) {
        Some(c) if c.len() == 2 => (c[0].as_f64().unwrap_or(cw / 2.0), c[1].as_f64().unwrap_or(ch / 2.0)),
        _ => (cw / 2.0, ch / 2.0),
    };
    let (dx, dy) = ((center.0 - w * scale / 2.0).round(), (center.1 - h * scale / 2.0).round());
    let mut px = flattened(&src, fmt);
    if (scale - 1.0).abs() > 1e-9 {
        px = resize_surface(&px, scale, scale, Resample::Bicubic);
    }
    let px = translate_surface(&px, dx as i32, dy as i32);
    let source = match linked {
        Some(path) => SmartSource::Linked { path },
        None => SmartSource::Embedded { file_name: file_name(name), bytes: Arc::new(bytes) },
    };
    let so = SmartObject::new(source, Affine { m: [scale, 0.0, 0.0, scale, dx, dy] }, Some(px));
    let layer_name = stem(name);
    let label = if matches!(so.source, SmartSource::Linked { .. }) { "Place Linked" } else { "Place Embedded" };
    let id = s.edit(label, |doc, active| {
        let id = doc.insert_above(*active, Layer::new(layer_name, LayerContent::Smart(so)));
        *active = Some(id);
        Ok(id)
    })?;
    Ok(json!({"layer": id.0, "scale": scale * 100.0, "bounds": [dx, dy, dx + w * scale, dy + h * scale]}))
}

fn place(s: &mut Session, p: &Value, linked: bool) -> Result<Value> {
    let cmd = if linked { "file.placeLinked" } else { "file.placeEmbedded" };
    let path = str_param(p, "path", cmd)?.to_string();
    let bytes = read_file(&path)?;
    place_bytes(s, &path, bytes, linked.then(|| path.clone()), p)
}

// ---------- File Info (XMP) ----------

/// The File Info fields we edit, as (JSON key, XMP property, container kind).
const INFO_FIELDS: [(&str, &str, &str); 6] = [
    ("title", "dc:title", "Alt"),
    ("author", "dc:creator", "Seq"),
    ("description", "dc:description", "Alt"),
    ("keywords", "dc:subject", "Bag"),
    ("copyright", "dc:rights", "Alt"),
    ("authorTitle", "photoshop:AuthorsPosition", ""),
];

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The element `<tag …>…</tag>` (first occurrence) as (start, end, inner).
fn find_element<'a>(xmp: &'a str, tag: &str) -> Option<(usize, usize, &'a str)> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = xmp[from..].find(&open).map(|i| i + from) {
        let after = xmp[i + open.len()..].chars().next();
        if matches!(after, Some('>' | ' ' | '/' | '\n' | '\r' | '\t')) {
            let gt = xmp[i..].find('>')? + i;
            if xmp[..gt].ends_with('/') {
                return Some((i, gt + 1, ""));
            }
            let close = format!("</{tag}>");
            let c = xmp[gt..].find(&close)? + gt;
            return Some((i, c + close.len(), &xmp[gt + 1..c]));
        }
        from = i + open.len();
    }
    None
}

/// `rdf:li` values of a container (or the element text for simple properties).
fn li_values(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some((a, b, v)) = find_element(rest, "rdf:li") {
        out.push(xml_unescape(v.trim()));
        rest = &rest[b..];
        let _ = a;
    }
    if out.is_empty() && !inner.contains('<') && !inner.trim().is_empty() {
        out.push(xml_unescape(inner.trim()));
    }
    out
}

/// Attribute form `prop="value"` on an `rdf:Description`.
fn find_attr<'a>(xmp: &'a str, prop: &str) -> Option<(usize, usize, &'a str)> {
    let key = format!(" {prop}=\"");
    let i = xmp.find(&key)?;
    let v0 = i + key.len();
    let v1 = xmp[v0..].find('"')? + v0;
    Some((i, v1 + 1, &xmp[v0..v1]))
}

/// File Info fields read from an XMP packet.
pub fn read_file_info(xmp: Option<&str>) -> Value {
    let mut m = serde_json::Map::new();
    let x = xmp.unwrap_or("");
    for (key, prop, kind) in INFO_FIELDS {
        let vals = match find_element(x, prop) {
            Some((_, _, inner)) => li_values(inner),
            None => find_attr(x, prop).map(|(_, _, v)| vec![xml_unescape(v)]).unwrap_or_default(),
        };
        let v = if kind == "Bag" {
            json!(vals)
        } else if kind == "Seq" {
            json!(vals.join("; "))
        } else {
            json!(vals.into_iter().next().unwrap_or_default())
        };
        m.insert(key.into(), v);
    }
    let marked =
        find_element(x, "xmpRights:Marked").map(|(_, _, v)| v.trim().to_string()).or_else(|| find_attr(x, "xmpRights:Marked").map(|(_, _, v)| v.to_string()));
    m.insert(
        "copyrightStatus".into(),
        json!(match marked.as_deref() {
            Some("True") => "copyrighted",
            Some("False") => "publicDomain",
            _ => "unknown",
        }),
    );
    let url = find_element(x, "xmpRights:WebStatement")
        .map(|(_, _, v)| xml_unescape(v.trim()))
        .or_else(|| find_attr(x, "xmpRights:WebStatement").map(|(_, _, v)| xml_unescape(v)));
    m.insert("copyrightUrl".into(), json!(url.unwrap_or_default()));
    Value::Object(m)
}

/// Rewrites the File Info fields of an XMP packet (creating one when there is none), keeping
/// every other property. Edited properties move into their own `rdf:Description` (several
/// descriptions per packet are valid XMP).
pub fn write_file_info(xmp: Option<&str>, info: &Value) -> String {
    let mut cur = read_file_info(xmp);
    if let (Some(c), Some(n)) = (cur.as_object_mut(), info.as_object()) {
        for (k, v) in n {
            if c.contains_key(k) {
                c.insert(k.clone(), v.clone());
            }
        }
    }
    let mut x = xmp.filter(|s| s.contains("</rdf:RDF>")).map(str::to_string).unwrap_or_else(|| {
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>".to_string()
    });
    let props: Vec<&str> = INFO_FIELDS.iter().map(|f| f.1).chain(["xmpRights:Marked", "xmpRights:WebStatement"]).collect();
    for prop in &props {
        while let Some((a, b, _)) = find_element(&x, prop) {
            x.replace_range(a..b, "");
        }
        while let Some((a, b, _)) = find_attr(&x, prop) {
            x.replace_range(a..b, "");
        }
    }
    let get = |k: &str| cur.get(k).cloned().unwrap_or(Value::Null);
    let mut body = String::new();
    for (key, prop, kind) in INFO_FIELDS {
        let vals: Vec<String> = match get(key) {
            Value::Array(a) => a.iter().filter_map(|v| v.as_str()).map(str::to_string).filter(|v| !v.is_empty()).collect(),
            Value::String(s) if kind == "Bag" || kind == "Seq" => s.split([';', ',']).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect(),
            Value::String(s) if !s.is_empty() => vec![s],
            _ => Vec::new(),
        };
        if vals.is_empty() {
            continue;
        }
        let items: String = vals
            .iter()
            .map(|v| {
                if kind == "Alt" { format!("<rdf:li xml:lang=\"x-default\">{}</rdf:li>", xml_escape(v)) } else { format!("<rdf:li>{}</rdf:li>", xml_escape(v)) }
            })
            .collect();
        if kind.is_empty() {
            body.push_str(&format!("   <{prop}>{}</{prop}>\n", xml_escape(&vals[0])));
        } else {
            body.push_str(&format!("   <{prop}><rdf:{kind}>{items}</rdf:{kind}></{prop}>\n"));
        }
    }
    match get("copyrightStatus").as_str() {
        Some("copyrighted") => body.push_str("   <xmpRights:Marked>True</xmpRights:Marked>\n"),
        Some("publicDomain") => body.push_str("   <xmpRights:Marked>False</xmpRights:Marked>\n"),
        _ => {}
    }
    if let Some(u) = get("copyrightUrl").as_str().filter(|u| !u.is_empty()) {
        body.push_str(&format!("   <xmpRights:WebStatement>{}</xmpRights:WebStatement>\n", xml_escape(u)));
    }
    if !body.is_empty() {
        let desc = format!(
            "  <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\" xmlns:xmpRights=\"http://ns.adobe.com/xap/1.0/rights/\">\n{body}  </rdf:Description>\n "
        );
        let at = x.rfind("</rdf:RDF>").unwrap_or(x.len());
        x.insert_str(at, &desc);
    }
    x
}

fn file_info(s: &mut Session, p: &Value) -> Result<Value> {
    let keys = ["title", "author", "authorTitle", "description", "keywords", "copyright", "copyrightStatus", "copyrightUrl"];
    let edits = p.as_object().is_some_and(|m| m.keys().any(|k| keys.contains(&k.as_str())));
    if edits {
        let xmp = s.active().ok_or(EngineError::NoDocument)?.doc.metadata.xmp.clone();
        let new = write_file_info(xmp.as_deref(), p);
        s.edit("File Info", |doc, _| {
            doc.metadata.xmp = Some(new);
            Ok(())
        })?;
    }
    let d = s.active().ok_or(EngineError::NoDocument)?;
    Ok(read_file_info(d.doc.metadata.xmp.as_deref()))
}

// ---------- Automate ----------

fn fit_image(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (w, h) = (d.doc.size.width as f64, d.doc.size.height as f64);
    let bw = f64_param(p, "width").unwrap_or(w).max(1.0);
    let bh = f64_param(p, "height").unwrap_or(h).max(1.0);
    let k = (bw / w).min(bh / h);
    if (k > 1.0 && p.get("dontEnlarge").and_then(Value::as_bool).unwrap_or(false)) || ((w * k).round() == w && (h * k).round() == h) {
        return Ok(json!({"width": w, "height": h, "changed": false}));
    }
    let (nw, nh) = ((w * k).round().max(1.0), (h * k).round().max(1.0));
    let resample = p.get("resample").and_then(Value::as_str).unwrap_or("bicubic");
    s.execute("image.imageSize", json!({"width": nw, "height": nh, "resample": resample}))?;
    Ok(json!({"width": nw, "height": nh, "changed": true}))
}

fn mode_name(m: ColorMode) -> &'static str {
    match m {
        ColorMode::Bitmap => "bitmap",
        ColorMode::Grayscale => "grayscale",
        ColorMode::Duotone => "duotone",
        ColorMode::Indexed => "indexed",
        ColorMode::Rgb => "rgb",
        ColorMode::Cmyk => "cmyk",
        ColorMode::Lab => "lab",
        ColorMode::Multichannel => "multichannel",
    }
}

fn conditional_mode_change(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.automate.conditionalModeChange";
    let to = str_param(p, "to", cmd)?.to_ascii_lowercase();
    let to = match to.as_str() {
        "gray" | "grayscale" => "grayscale",
        "rgb" => "rgb",
        "cmyk" => "cmyk",
        "lab" => "lab",
        other => return Err(EngineError::BadParams { cmd: cmd.into(), msg: format!("unsupported target mode \"{other}\" (rgb, grayscale, cmyk, lab)") }),
    };
    let cur = mode_name(s.active().ok_or(EngineError::NoDocument)?.doc.mode);
    let from: Vec<String> = match p.get("from") {
        Some(Value::Array(a)) => {
            a.iter().filter_map(Value::as_str).map(|v| v.to_ascii_lowercase().replace("gray", "grayscale").replace("grayscalescale", "grayscale")).collect()
        }
        Some(Value::String(v)) if v != "any" => vec![v.to_ascii_lowercase()],
        _ => vec!["any".into()],
    };
    if cur == to || !(from.iter().any(|f| f == "any" || f == cur)) {
        return Ok(json!({"changed": false, "mode": cur}));
    }
    s.execute(&format!("image.mode.{to}"), json!({}))?;
    Ok(json!({"changed": true, "mode": to}))
}

/// One `[id, params]` / `{command, params}` / `{id, params}` step of an action.
fn parse_steps(v: &Value) -> Result<Vec<(String, Value)>> {
    let bad = |m: &str| EngineError::BadParams { cmd: "file.automate.batch".into(), msg: m.into() };
    let arr = v.as_array().ok_or_else(|| bad("\"steps\" must be an array"))?;
    arr.iter()
        .map(|st| match st {
            Value::Array(a) if !a.is_empty() => {
                Ok((a[0].as_str().ok_or_else(|| bad("step id must be a string"))?.to_string(), a.get(1).cloned().unwrap_or(json!({}))))
            }
            Value::Object(o) => {
                let id = o.get("command").or_else(|| o.get("id")).and_then(Value::as_str).ok_or_else(|| bad("step needs \"command\""))?;
                Ok((id.to_string(), o.get("params").cloned().unwrap_or(json!({}))))
            }
            Value::String(id) => Ok((id.clone(), json!({}))),
            _ => Err(bad("each step is [id, params] or {\"command\", \"params\"}")),
        })
        .collect()
}

/// Inputs of a batch command: `"input"` is a folder or an array of files.
pub(crate) fn batch_inputs(p: &Value, cmd: &str) -> Result<Vec<String>> {
    match p.get("input").or_else(|| p.get("files")) {
        Some(Value::Array(a)) => Ok(a.iter().filter_map(Value::as_str).map(str::to_string).collect()),
        Some(Value::String(dir)) => list_images(dir),
        _ => Err(EngineError::BadParams { cmd: cmd.into(), msg: "missing \"input\" (folder or array of files)".into() }),
    }
}

/// Opens each input in a scratch session, runs `f` on it and saves it to `output` as `format`
/// (`"same"` keeps the input's extension). Errors per file are collected, not fatal.
pub(crate) fn process_files(
    inputs: &[String],
    output: &str,
    format: &str,
    quality: Option<f64>,
    suffix: &str,
    f: &dyn Fn(&mut Session) -> Result<()>,
) -> Value {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for path in inputs {
        let r = (|| -> Result<String> {
            let bytes = read_file(path)?;
            let mut scratch = Session::new();
            let doc = import(&file_name(path), &bytes)?;
            scratch.add_document(doc, Some(path.clone()));
            f(&mut scratch)?;
            let ext = if format == "same" {
                path.rsplit('.').next().unwrap_or("png").to_ascii_lowercase()
            } else {
                format.trim_start_matches('.').to_ascii_lowercase()
            };
            let out = join(output, &format!("{}{suffix}.{ext}", stem(path)));
            let d = scratch.active().ok_or(EngineError::NoDocument)?;
            save_doc(&d.doc, &out, quality)?;
            Ok(out)
        })();
        match r {
            Ok(out) => files.push(out),
            Err(e) => errors.push(json!({"file": path, "error": e.to_string()})),
        }
    }
    json!({"files": files, "errors": errors})
}

fn batch(_s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.automate.batch";
    let steps =
        parse_steps(p.get("steps").or_else(|| p.get("action")).ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: "missing \"steps\"".into() })?)?;
    if let Some((id, _)) = steps.iter().find(|(id, _)| crate::commands::find(id).is_none()) {
        return Err(EngineError::BadParams { cmd: cmd.into(), msg: format!("unknown command `{id}` in the action") });
    }
    let inputs = batch_inputs(p, cmd)?;
    let output = str_param(p, "output", cmd)?.to_string();
    let format = p.get("format").and_then(Value::as_str).unwrap_or("same").to_string();
    let r = process_files(&inputs, &output, &format, f64_param(p, "quality"), "", &|scratch| {
        for (id, params) in &steps {
            scratch.execute(id, params.clone())?;
        }
        Ok(())
    });
    Ok(r)
}

fn image_processor(_s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.scripts.imageProcessor";
    let inputs = batch_inputs(p, cmd)?;
    let output = str_param(p, "output", cmd)?.to_string();
    let format = p.get("format").and_then(Value::as_str).unwrap_or("jpg").to_string();
    let fit = match (size_param(p, "width"), size_param(p, "height")) {
        (None, None) => None,
        (w, h) => Some(json!({"width": w.unwrap_or(1e9), "height": h.unwrap_or(1e9), "dontEnlarge": true})),
    };
    let to_srgb = p.get("convertToSrgb").and_then(Value::as_bool).unwrap_or(false);
    let r = process_files(&inputs, &output, &format, f64_param(p, "quality").or(Some(8.0)), "", &|scratch| {
        if to_srgb && scratch.active().is_some_and(|d| d.doc.mode != ColorMode::Rgb) {
            scratch.execute("image.mode.rgb", json!({}))?;
        }
        if let Some(fp) = &fit {
            fit_image(scratch, fp)?;
        }
        Ok(())
    });
    Ok(r)
}

fn load_files_into_stack(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.scripts.loadFilesIntoStack";
    let paths: Vec<String> = match p.get("paths").or_else(|| p.get("input")) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(dir)) => list_images(dir)?,
        _ => return Err(EngineError::BadParams { cmd: cmd.into(), msg: "missing \"paths\"".into() }),
    };
    if paths.is_empty() {
        return Err(EngineError::BadParams { cmd: cmd.into(), msg: "no files to load".into() });
    }
    let mut docs = Vec::new();
    for path in &paths {
        let bytes = read_file(path)?;
        docs.push((file_name(path), import(&file_name(path), &bytes)?));
    }
    let first = &docs[0].1;
    let w = docs.iter().map(|(_, d)| d.size.width).max().unwrap_or(1);
    let h = docs.iter().map(|(_, d)| d.size.height).max().unwrap_or(1);
    let mut stack = Document::new(stem(&paths[0]), photocraft_doc::Size::new(w, h), first.mode, first.depth);
    stack.resolution_dpi = first.resolution_dpi;
    stack.icc_profile = first.icc_profile.clone();
    let fmt = stack.pixel_format();
    // First file at the bottom, like Photoshop's script.
    for (name, d) in &docs {
        stack.layers.push(Layer::new(name.clone(), LayerContent::Raster(flattened(d, fmt))));
    }
    let n = stack.layers.len();
    if p.get("createSmartObject").and_then(Value::as_bool).unwrap_or(false) {
        // "Create Smart Object after Loading Layers": the layers go inside one smart object, ready
        // for Layer › Smart Objects › Stack Mode.
        let children = std::mem::take(&mut stack.layers);
        let group = Layer::new(stem(&paths[0]), LayerContent::Group(photocraft_doc::Group { children, expanded: true, artboard: None }));
        let smart = crate::smart_cmds::layer_to_smart(&stack, &group)?;
        stack.layers = vec![smart];
    }
    let i = s.add_document(stack, None);
    Ok(json!({"document": i, "layers": n}))
}

// ---------- Scripts: flatten effects / masks ----------

fn has_live_effects(l: &Layer) -> bool {
    l.effects.enabled && l.effects.items.iter().any(|e| e.enabled())
}

/// The layer rendered alone (keeping what `keep` says), as a surface in `fmt`.
fn bake(l: &Layer, canvas: Rect, fmt: PixelFormat, keep_effects: bool) -> Surface {
    let mut tmp = l.clone();
    tmp.opacity = 1.0;
    tmp.blend = photocraft_color::BlendMode::Normal;
    tmp.visible = true;
    tmp.clipped = false;
    if !keep_effects {
        tmp.effects.items.clear();
        tmp.fill_opacity = 1.0;
    }
    // Render over the union of the canvas and the content so off-canvas pixels survive.
    let area = l.surface().map_or(canvas, |s| s.content_bounds().union(&canvas));
    let area = if keep_effects { area.inflate(256) } else { area };
    buffer_surface(&photocraft_compose::render_layer(&tmp, area), fmt)
}

fn walk_ids(doc: &Document) -> Vec<LayerId> {
    doc.walk().into_iter().map(|(_, _, l)| l.id).collect()
}

fn flatten_all_effects(s: &mut Session) -> Result<Value> {
    let mut n = 0;
    s.edit("Flatten All Layer Effects", |doc, _| {
        let canvas = doc.bounds();
        let fmt = doc.pixel_format();
        for id in walk_ids(doc) {
            let Some(l) = doc.layer(id) else { continue };
            if !has_live_effects(l) || matches!(l.content, LayerContent::Group(_) | LayerContent::Adjustment(_)) {
                continue;
            }
            let px = bake(l, canvas, fmt, true);
            let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
            l.content = LayerContent::Raster(px);
            l.effects = Default::default();
            l.mask = None;
            l.vector_mask = None;
            l.fill_opacity = 1.0;
            l.fill_cache = None;
            l.psd_blocks.retain(|(k, _)| !matches!(k, b"TySh" | b"SoLd" | b"PlLd" | b"SoLE" | b"vmsk" | b"vsms" | b"vogk" | b"vscg" | b"vstk"));
            n += 1;
        }
        Ok(())
    })?;
    Ok(json!({"flattened": n}))
}

fn flatten_all_masks(s: &mut Session) -> Result<Value> {
    let (mut n, mut skipped) = (0, 0);
    s.edit("Flatten All Masks", |doc, _| {
        let canvas = doc.bounds();
        let fmt = doc.pixel_format();
        for id in walk_ids(doc) {
            let Some(l) = doc.layer(id) else { continue };
            let masked = l.mask.as_ref().is_some_and(|m| m.enabled) || l.vector_mask.is_some();
            if !masked {
                continue;
            }
            if !matches!(l.content, LayerContent::Raster(_)) {
                // Masks on groups, adjustment, type, shape and smart layers have no pixels to
                // fold into; Photoshop's script leaves them too.
                skipped += 1;
                continue;
            }
            let px = bake(l, canvas, fmt, false);
            let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
            l.content = LayerContent::Raster(px);
            l.mask = None;
            l.vector_mask = None;
            l.psd_blocks.retain(|(k, _)| !matches!(k, b"vmsk" | b"vsms"));
            n += 1;
        }
        Ok(())
    })?;
    Ok(json!({"flattened": n, "skipped": skipped}))
}

// ---------- Export ----------

fn layers_to_files(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.export.layersToFiles";
    let dir = str_param(p, "dir", cmd).or_else(|_| str_param(p, "output", cmd))?.to_string();
    let format = p.get("format").and_then(Value::as_str).unwrap_or("png").trim_start_matches('.').to_ascii_lowercase();
    let visible_only = p.get("visibleOnly").and_then(Value::as_bool).unwrap_or(true);
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let prefix = p.get("prefix").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| stem(&d.doc.name));
    let doc = d.doc.clone();
    let mut files = Vec::new();
    // Top-level layers (groups export merged), top to bottom like Photoshop's numbering.
    let layers: Vec<&Layer> = doc.layers.iter().rev().filter(|l| l.visible || !visible_only).collect();
    for (i, l) in layers.iter().enumerate() {
        if matches!(l.content, LayerContent::Adjustment(_)) {
            continue;
        }
        let mut one = (*doc).clone();
        let mut only = (*l).clone();
        only.visible = true;
        only.clipped = false;
        one.layers = vec![only];
        let path = join(&dir, &format!("{}_{:04}_{}.{format}", sanitize(&prefix), i, sanitize(&l.name)));
        save_doc(&one, &path, f64_param(p, "quality"))?;
        files.push(path);
    }
    Ok(json!({"files": files}))
}

/// The document's visible top-level adjustment layers applied to an identity lattice, as a
/// `.cube` 3D LUT (red fastest, values 0–1).
pub fn bake_cube(doc: &Document, size: usize, title: &str) -> String {
    let n = size.clamp(2, 256);
    let (w, h) = ((n * n) as u32, n as u32);
    let mut lattice = Document::new("lut", photocraft_doc::Size::new(w, h), ColorMode::Rgb, photocraft_color::SampleType::F32);
    let fmt = lattice.pixel_format();
    let step = 1.0 / (n - 1) as f32;
    let mut data = Vec::with_capacity((w * h) as usize * fmt.channels());
    for g in 0..n {
        for b in 0..n {
            for r in 0..n {
                let mut px = [0.0f32; 8];
                let k = from_rgba_into(&fmt, [r as f32 * step, g as f32 * step, b as f32 * step, 1.0], &mut px);
                data.extend_from_slice(&px[..k]);
            }
        }
    }
    let mut surf = Surface::new(fmt);
    surf.write_region(Rect::new(0, 0, w as i32, h as i32), &data);
    lattice.layers.push(Layer::new("Lattice", LayerContent::Raster(surf)));
    for l in doc.layers.iter().filter(|l| l.visible && matches!(l.content, LayerContent::Adjustment(_))) {
        let mut a = l.clone();
        // Masks and clipping are spatial; a LUT is the adjustment stack's colour mapping.
        a.mask = None;
        a.vector_mask = None;
        a.clipped = false;
        lattice.layers.push(a);
    }
    let out = photocraft_compose::flatten(&lattice);
    let mut s = format!("TITLE \"{}\"\n# Created by Photocraft\nLUT_3D_SIZE {n}\nDOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\n", title.replace('"', "'"));
    // .cube order: red changes fastest, then green, then blue.
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let (x, y) = (b * n + r, g);
                let px = out.px[y * w as usize + x];
                s.push_str(&format!("{:.6} {:.6} {:.6}\n", px[0].clamp(0.0, 1.0), px[1].clamp(0.0, 1.0), px[2].clamp(0.0, 1.0)));
            }
        }
    }
    s
}

fn color_lookup_tables(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let size = p.get("size").and_then(Value::as_u64).unwrap_or(33) as usize;
    let title = p.get("title").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| stem(&d.doc.name));
    let cube = bake_cube(&d.doc, size, &title);
    match p.get("path").and_then(Value::as_str) {
        Some(path) => {
            write_file(path, cube.as_bytes())?;
            Ok(json!({"path": path, "size": size.clamp(2, 256)}))
        }
        None => Ok(json!({"cube": cube, "size": size.clamp(2, 256)})),
    }
}

// ---------- guides ----------

fn push_unique(v: &mut Vec<f32>, x: f64) {
    let x = x as f32;
    if !v.iter().any(|g| (g - x).abs() < 0.01) {
        v.push(x);
    }
}

/// Guide positions for `count` columns (or rows) across `[start, end]`, Photoshop-style: both
/// edges of every column; a fixed `width` packs columns from `start` (or centres them).
fn layout_lines(start: f64, end: f64, count: u32, width: Option<f64>, gutter: f64, center: bool, out: &mut Vec<f32>) {
    let avail = (end - start).max(0.0);
    if count == 0 {
        return;
    }
    let n = count as f64;
    let w = width.unwrap_or(((avail - (n - 1.0) * gutter) / n).max(0.0));
    let total = n * w + (n - 1.0) * gutter;
    let x0 = if center && width.is_some() { start + (avail - total) / 2.0 } else { start };
    for i in 0..count {
        let a = x0 + i as f64 * (w + gutter);
        push_unique(out, a);
        push_unique(out, a + w);
    }
}

fn new_guide_layout(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (cw, ch) = (d.doc.size.width as f64, d.doc.size.height as f64);
    let m = match p.get("margin") {
        Some(Value::Array(a)) if a.len() == 4 => [0, 1, 2, 3].map(|i| a[i].as_f64().unwrap_or(0.0)),
        Some(v) if v.is_number() => [v.as_f64().unwrap_or(0.0); 4],
        _ => [0.0; 4],
    };
    let [top, left, bottom, right] = m;
    let cols = p.get("columns").and_then(Value::as_u64).unwrap_or(0) as u32;
    let rows = p.get("rows").and_then(Value::as_u64).unwrap_or(0) as u32;
    let mut v: Vec<f32> = Vec::new();
    let mut h: Vec<f32> = Vec::new();
    let has_margin = m.iter().any(|x| *x != 0.0);
    if has_margin {
        push_unique(&mut v, left);
        push_unique(&mut v, cw - right);
        push_unique(&mut h, top);
        push_unique(&mut h, ch - bottom);
    }
    let center = p.get("centerColumns").and_then(Value::as_bool).unwrap_or(false);
    layout_lines(left, cw - right, cols, size_param(p, "width"), f64_param(p, "gutter").unwrap_or(0.0), center, &mut v);
    layout_lines(top, ch - bottom, rows, size_param(p, "height"), f64_param(p, "rowGutter").or_else(|| f64_param(p, "gutter")).unwrap_or(0.0), center, &mut h);
    if v.is_empty() && h.is_empty() {
        return Err(EngineError::BadParams { cmd: "view.newGuideLayout".into(), msg: "give \"columns\", \"rows\" or a \"margin\"".into() });
    }
    let clear = p.get("clearExisting").and_then(Value::as_bool).unwrap_or(false);
    let (nv, nh) = (v.len(), h.len());
    s.edit("New Guide Layout", |doc, _| {
        if clear {
            doc.guides.vertical.clear();
            doc.guides.horizontal.clear();
        }
        for x in v {
            push_unique(&mut doc.guides.vertical, x as f64);
        }
        for y in h {
            push_unique(&mut doc.guides.horizontal, y as f64);
        }
        Ok(())
    })?;
    Ok(json!({"vertical": nv, "horizontal": nh}))
}

fn guides_from_shape(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_param(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let LayerContent::Shape(sh) = &l.content else { return Err(EngineError::Other("the layer is not a shape layer".into())) };
    let (x0, y0, x1, y1) = sh.path.control_bounds().ok_or(EngineError::Other("the shape has no path".into()))?;
    s.edit("New Guides From Shape", |doc, _| {
        for x in [x0, (x0 + x1) / 2.0, x1] {
            push_unique(&mut doc.guides.vertical, x);
        }
        for y in [y0, (y0 + y1) / 2.0, y1] {
            push_unique(&mut doc.guides.horizontal, y);
        }
        Ok(())
    })?;
    Ok(json!({"bounds": [x0, y0, x1, y1]}))
}

fn clear_canvas_guides(s: &mut Session) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    // Guides inside an artboard belong to it (View › Clear Selected Artboard Guides); the rest
    // are canvas guides. Without artboards every guide is a canvas guide.
    let boards: Vec<Rect> = d.doc.artboards().iter().map(|b| b.2.rect).collect();
    let in_x = move |x: f32| boards.iter().any(|r| f64::from(x) > f64::from(r.x0) && f64::from(x) < f64::from(r.x1));
    let boards_y: Vec<Rect> = d.doc.artboards().iter().map(|b| b.2.rect).collect();
    let in_y = move |y: f32| boards_y.iter().any(|r| f64::from(y) > f64::from(r.y0) && f64::from(y) < f64::from(r.y1));
    let n = d.doc.guides.vertical.iter().filter(|x| !in_x(**x)).count() + d.doc.guides.horizontal.iter().filter(|y| !in_y(**y)).count();
    if n == 0 {
        return Ok(json!({"cleared": 0}));
    }
    s.edit("Clear Canvas Guides", |doc, _| {
        doc.guides.vertical.retain(|x| in_x(*x));
        doc.guides.horizontal.retain(|y| in_y(*y));
        Ok(())
    })?;
    Ok(json!({"cleared": n}))
}

// ---------- registry ----------

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $sc:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: $sc, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    vec![
        spec!("file.closeAll", "Close All", &["File"], Some("Cmd+Alt+W"), "{}", has_doc, |s, _| close_all(s)),
        spec!(
            "file.closeOthers",
            "Close Others",
            &["File"],
            Some("Cmd+Alt+P"),
            r##"{"document":index? (the one to keep; default active)}"##,
            has_doc,
            close_others
        ),
        spec!("file.revert", "Revert", &["File"], Some("F12"), "{} (reloads the saved file as one undoable step)", can_revert, |s, _| revert(s)),
        spec!(
            "file.saveACopy",
            "Save a Copy…",
            &["File"],
            Some("Cmd+Alt+S"),
            r##"{"path":str (format from the extension),"quality":0..12? (JPEG),"layers":bool=true}"##,
            native_doc,
            save_a_copy
        ),
        spec!(
            "file.openAs",
            "Open As…",
            &["File"],
            Some("Cmd+Alt+Shift+O"),
            r##"{"path":str,"as":"psd|png|jpg|tiff|…"? (decode as this format)}"##,
            native,
            open_as
        ),
        spec!(
            "file.placeEmbedded",
            "Place Embedded…",
            &["File"],
            None,
            r##"{"path":str,"scale":%? (default: fit when larger than the canvas),"fit":bool=true,"center":[x,y]?}"##,
            native_doc,
            |s, p| place(s, p, false)
        ),
        spec!("file.placeLinked", "Place Linked…", &["File"], None, r##"{"path":str,"scale":%?,"fit":bool=true,"center":[x,y]?}"##, native_doc, |s, p| place(
            s, p, true
        )),
        spec!(
            "file.fileInfo",
            "File Info…",
            &["File"],
            Some("Cmd+Alt+Shift+I"),
            r##"{"title":str?,"author":str?,"authorTitle":str?,"description":str?,"keywords":[str]|"a; b"?,"copyright":str?,"copyrightStatus":"unknown|copyrighted|publicDomain"?,"copyrightUrl":str?} (no keys: read)"##,
            has_doc,
            file_info
        ),
        spec!(
            "file.automate.fitImage",
            "Fit Image…",
            &["File", "Automate"],
            None,
            r##"{"width":px,"height":px,"dontEnlarge":bool=false,"resample":"bicubic|bilinear|nearest|lanczos|preserveDetails"="bicubic"}"##,
            has_doc,
            fit_image
        ),
        spec!(
            "file.automate.conditionalModeChange",
            "Conditional Mode Change…",
            &["File", "Automate"],
            None,
            r##"{"from":["rgb","grayscale","cmyk","lab","indexed","bitmap",…]|"any"="any","to":"rgb|grayscale|cmyk|lab"}"##,
            has_doc,
            conditional_mode_change
        ),
        spec!(
            "file.automate.batch",
            "Batch…",
            &["File", "Automate"],
            None,
            r##"{"steps":[[commandId,params]|{"command":id,"params":{}}…] (a recorded action),"input":folder|[paths],"output":folder,"format":"same|png|jpg|psd|tiff|…"="same","quality":0..12?} → {files, errors}"##,
            native,
            batch
        ),
        spec!(
            "file.scripts.imageProcessor",
            "Image Processor…",
            &["File", "Scripts"],
            None,
            r##"{"input":folder|[paths],"output":folder,"format":"jpg|png|psd|tiff|…"="jpg","quality":0..12=8,"width":px?,"height":px? (fit, never enlarge),"convertToSrgb":bool=false} → {files, errors}"##,
            native,
            image_processor
        ),
        spec!(
            "file.scripts.loadFilesIntoStack",
            "Load Files into Stack…",
            &["File", "Scripts"],
            None,
            r##"{"paths":[str]|folder,"createSmartObject":bool=false} → new document with one layer per file (inside one smart object with createSmartObject)"##,
            native,
            load_files_into_stack
        ),
        spec!("file.scripts.flattenAllLayerEffects", "Flatten All Layer Effects", &["File", "Scripts"], None, "{}", has_doc, |s, _| flatten_all_effects(s)),
        spec!(
            "file.scripts.flattenAllMasks",
            "Flatten All Masks",
            &["File", "Scripts"],
            None,
            "{} (pixel layers; masks on other layer kinds are left)",
            has_doc,
            |s, _| flatten_all_masks(s)
        ),
        spec!(
            "file.export.layersToFiles",
            "Layers to Files…",
            &["File", "Export"],
            None,
            r##"{"dir":folder,"format":"png|jpg|psd|tiff|…"="png","prefix":str=document name,"visibleOnly":bool=true,"quality":0..12?} → {files}"##,
            native_doc,
            layers_to_files
        ),
        spec!(
            "file.export.colorLookupTables",
            "Color Lookup Tables…",
            &["File", "Export"],
            None,
            r##"{"path":str? (.cube; omit to return the text),"size":2..256=33,"title":str?}"##,
            has_adjustments,
            color_lookup_tables
        ),
        spec!(
            "view.newGuideLayout",
            "New Guide Layout…",
            &["View"],
            None,
            r##"{"columns":n=0,"width":px?,"gutter":px=0,"rows":n=0,"height":px?,"rowGutter":px=gutter,"margin":px|[top,left,bottom,right]=0,"centerColumns":bool=false,"clearExisting":bool=false}"##,
            has_doc,
            new_guide_layout
        ),
        spec!("view.newGuidesFromShape", "New Guides From Shape", &["View"], None, r##"{"layer":id?}"##, has_shape, guides_from_shape),
        spec!("view.clearCanvasGuides", "Clear Canvas Guides", &["View"], None, "{}", has_doc, |s, _| clear_canvas_guides(s)),
    ]
}

#[cfg(test)]
#[path = "file_cmds/tests.rs"]
mod tests;
