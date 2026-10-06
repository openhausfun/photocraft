//! Filter › Lens Correction, Filter › Adaptive Wide Angle, Filter › Camera Raw Filter and
//! File › Automate › Lens Correction (the batch twin).
//!
//! The three filters act on the active pixel layer (inside the selection) or, on a smart
//! object, record a smart filter whose params are fully resolved (EXIF-derived values are
//! written into them), so re-rendering is deterministic. Algorithms: [`photocraft_algo::lens`],
//! [`photocraft_algo::wideangle`], [`photocraft_algo::camera_raw`].

use photocraft_algo::camera_raw::{self, CameraRaw};
use photocraft_algo::exif;
use photocraft_algo::lens::{self, EdgeMode, LensCorrection};
use photocraft_algo::transform::Interp;
use photocraft_algo::wideangle::{self, WideAngle};
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, LayerContent, LayerId, SmartFilter};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::photo_cmds::Stopwatch;
use crate::{EngineError, Result, Session};

pub const LENS: &str = "filter.lensCorrection";
pub const WIDE: &str = "filter.adaptiveWideAngle";
pub const RAW: &str = "filter.cameraRaw";
pub const LENS_BATCH: &str = "file.automate.lensCorrection";

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn num(p: &Value, k: &str, d: f64) -> f64 {
    p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(d)
}

fn flag(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}

// ---------- params ----------

/// Lens Correction settings from params; `exif` resolves `"profile":"auto"`.
pub fn lens_params(cmd: &str, p: &Value, frame: Rect, info: Option<&exif::CameraInfo>) -> Result<LensCorrection> {
    let mut lc = LensCorrection {
        correct_distortion: flag(p, "correctDistortion", true),
        correct_vignette: flag(p, "correctVignette", true),
        correct_ca: flag(p, "correctCA", true),
        distortion: num(p, "distortion", 0.0).clamp(-100.0, 100.0),
        red_cyan: num(p, "redCyan", 0.0).clamp(-100.0, 100.0),
        blue_yellow: num(p, "blueYellow", 0.0).clamp(-100.0, 100.0),
        vignette_amount: num(p, "vignetteAmount", 0.0).clamp(-100.0, 100.0),
        vignette_midpoint: num(p, "vignetteMidpoint", 50.0).clamp(0.0, 100.0),
        vertical: num(p, "vertical", 0.0).clamp(-100.0, 100.0),
        horizontal: num(p, "horizontal", 0.0).clamp(-100.0, 100.0),
        angle: num(p, "angle", 0.0),
        scale: num(p, "scale", 100.0).clamp(50.0, 150.0),
        ..Default::default()
    };
    lc.edge = match p.get("edge").and_then(Value::as_str).unwrap_or("transparency") {
        "transparency" => EdgeMode::Transparency,
        "edgeExtension" | "extension" => EdgeMode::Extension,
        "black" => EdgeMode::Color([0.0, 0.0, 0.0, 1.0]),
        "white" => EdgeMode::Color([1.0, 1.0, 1.0, 1.0]),
        "background" | "color" => EdgeMode::Color(crate::commands::color_param(p, "edgeColor", [0.0, 0.0, 0.0, 1.0])),
        other => return Err(bad(cmd, format!("unknown edge `{other}` (transparency|edgeExtension|black|white|color)"))),
    };
    let focal = num(p, "focalLength", 0.0);
    lc.profile = match p.get("profile").and_then(Value::as_str).unwrap_or("none") {
        "none" | "" => None,
        "generic" => Some(lens::generic_profile(if focal > 0.0 { focal } else { 50.0 })),
        "auto" => info.and_then(|i| i.focal_length_35mm.or(i.focal_length)).or((focal > 0.0).then_some(focal)).map(lens::generic_profile),
        other => return Err(bad(cmd, format!("unknown profile `{other}` (none|auto|generic)"))),
    };
    if let Some(Value::Array(line)) = p.get("straighten") {
        let pt = |v: &Value| -> Option<[f64; 2]> { Some([v.get(0)?.as_f64()?, v.get(1)?.as_f64()?]) };
        let (Some(a), Some(b)) = (line.first().and_then(pt), line.get(1).and_then(pt)) else {
            return Err(bad(cmd, "`straighten` is [[x0,y0],[x1,y1]]"));
        };
        lc.angle += lens::straighten_angle(a, b);
    }
    if flag(p, "autoScale", false) {
        lc.scale = lens::auto_scale(&lc, frame);
    }
    Ok(lc)
}

/// Writes the resolved profile / angle / scale back into the params (for smart filters).
fn resolved_lens_params(p: &Value, lc: &LensCorrection) -> Value {
    let mut q = p.clone();
    if let Value::Object(m) = &mut q {
        m.remove("straighten");
        m.remove("autoScale");
        m.remove("layer");
        m.insert("angle".into(), json!(lc.angle));
        m.insert("scale".into(), json!(lc.scale));
        match &lc.profile {
            Some(pr) => {
                m.insert("profile".into(), json!("generic"));
                let f = pr.name.trim_start_matches("Generic Lens (").split(' ').next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(50.0);
                m.insert("focalLength".into(), json!(f));
            }
            None => {
                m.insert("profile".into(), json!("none"));
            }
        }
    }
    q
}

/// Adaptive Wide Angle settings; a zero focal length is taken from EXIF when present.
pub fn wide_params(cmd: &str, p: &Value, info: Option<&exif::CameraInfo>) -> Result<WideAngle> {
    let mut wa: WideAngle = serde_json::from_value(p.clone())
        .map_err(|e| bad(cmd, format!("bad params: {e} (constraints: [{{\"a\":[x,y],\"b\":[x,y],\"orientation\":\"free|horizontal|vertical\"}}])")))?;
    if wa.constraints.iter().any(|c| c.a.iter().chain(&c.b).any(|v| !v.is_finite())) {
        return Err(bad(cmd, "constraint points must be finite"));
    }
    wa.scale = wa.scale.clamp(50.0, 150.0);
    wa.crop_factor = wa.crop_factor.clamp(0.1, 10.0);
    if wa.focal_length <= 0.0
        && let Some(i) = info
    {
        if let Some(f) = i.focal_length {
            wa.focal_length = f;
            if let Some(f35) = i.focal_length_35mm {
                wa.crop_factor = (f35 / f).clamp(0.1, 10.0);
            }
        } else if let Some(f35) = i.focal_length_35mm {
            wa.focal_length = f35;
            wa.crop_factor = 1.0;
        }
    }
    Ok(wa)
}

/// Camera Raw settings (ACR names in camelCase).
pub fn raw_params(cmd: &str, p: &Value) -> Result<CameraRaw> {
    let mut q = p.clone();
    if let Value::Object(m) = &mut q {
        m.remove("layer");
    }
    serde_json::from_value(q).map_err(|e| bad(cmd, format!("bad params: {e}")))
}

// ---------- pixels ----------

fn rgba_region(surf: &Surface, area: Rect) -> Vec<[f32; 4]> {
    let mut v = vec![[0.0f32; 4]; (area.width() * area.height()) as usize];
    surf.read_rgba_into(area, &mut v);
    v
}

/// Camera Raw on a surface over `area` (RGB / Gray via RGBA, at the surface's depth).
pub fn camera_raw_surface(surf: &Surface, area: Rect, p: &CameraRaw) -> Surface {
    let area = area.intersect(&surf.content_bounds().union(&area));
    if area.is_empty() || p.is_identity() {
        return surf.clone();
    }
    let fmt = surf.format();
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut px = rgba_region(surf, area);
    camera_raw::develop(&mut px, w, h, p, fmt.sample == SampleType::F32);
    let n = fmt.channels();
    let orig = surf.read_region(area);
    let mut data = vec![0.0f32; w * h * n];
    for (i, (q, o)) in px.iter().zip(data.chunks_exact_mut(n)).enumerate() {
        from_rgba_into(&fmt, *q, o);
        if fmt.alpha {
            // Alpha is untouched.
            o[n - 1] = orig[i * n + n - 1];
        }
    }
    let mut out = surf.clone();
    out.write_region(area, &data);
    out.prune();
    out
}

/// Mixes `new` over `old` by the selection's coverage.
fn mix_by_selection(old: &Surface, new: &Surface, sel: &Surface) -> Surface {
    let area = old.content_bounds().union(&new.content_bounds());
    let fmt = new.format();
    let old = if old.format() == fmt { old.clone() } else { old.convert(fmt) };
    let mut out = new.clone();
    if area.is_empty() {
        return out;
    }
    let n = fmt.channels();
    let (o, w) = (old.read_region(area), new.read_region(area));
    let aw = area.width() as usize;
    let mut r = vec![0.0f32; o.len()];
    for (i, ((op, np), rp)) in o.chunks_exact(n).zip(w.chunks_exact(n)).zip(r.chunks_exact_mut(n)).enumerate() {
        let k = sel.sample_channel(area.x0 + (i % aw) as i32, area.y0 + (i / aw) as i32, 0).clamp(0.0, 1.0);
        if !fmt.alpha {
            for c in 0..n {
                rp[c] = op[c] + (np[c] - op[c]) * k;
            }
            continue;
        }
        let a = n - 1;
        let oa = op[a] + (np[a] - op[a]) * k;
        rp[a] = oa;
        for c in 0..a {
            let pm = op[c] * op[a] + (np[c] * np[a] - op[c] * op[a]) * k;
            rp[c] = if oa > 0.0 { pm / oa } else { 0.0 };
        }
    }
    out.write_region(area, &r);
    out.prune();
    out
}

/// Smart-filter hook: re-applies a stored filter of this module (`None` for other ids).
pub fn apply_to_surface(id: &str, params: &Value, surf: &Surface, canvas: Rect) -> Option<Surface> {
    match id {
        LENS => Some(lens::correct(surf, canvas, &lens_params(id, params, canvas, None).ok()?)),
        WIDE => Some(wideangle::apply(surf, canvas, &wide_params(id, params, None).ok()?, Interp::Bicubic)),
        RAW => Some(camera_raw_surface(surf, canvas, &raw_params(id, params).ok()?)),
        _ => None,
    }
}

// ---------- commands ----------

fn target(s: &Session, p: &Value) -> Result<LayerId> {
    match p.get("layer").and_then(Value::as_u64) {
        Some(v) => Ok(LayerId(v)),
        None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into())),
    }
}

fn camera_info(doc: &Document) -> Option<exif::CameraInfo> {
    doc.metadata.exif.as_ref().map(|e| exif::read(e))
}

fn filterable(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    let id = d.active_layer.ok_or("no active layer")?;
    match &d.doc.layer(id).ok_or("no active layer")?.content {
        LayerContent::Raster(_) => Ok(()),
        LayerContent::Smart(sm) if sm.cache.is_some() => Ok(()),
        other => Err(format!("filters need a pixel layer (active layer is a {} layer)", other.kind_name())),
    }
}

fn raw_enabled(s: &Session) -> std::result::Result<(), String> {
    filterable(s)?;
    let d = s.active().ok_or("no document open")?;
    match d.doc.mode {
        ColorMode::Rgb | ColorMode::Grayscale => Ok(()),
        m => Err(format!("Camera Raw Filter needs an RGB or Grayscale document (this one is {m:?})")),
    }
}

/// Runs `f` on the target layer (pixel layer edited in place inside the selection; smart object
/// gets a smart filter with `stored` params).
fn run_filter(s: &mut Session, cmd: &str, label: &str, stored: Value, float_bg: bool, f: &dyn Fn(&Surface, Rect) -> Surface) -> Result<LayerId> {
    let id = target(s, &stored)?;
    let mut stored = stored;
    if let Value::Object(m) = &mut stored {
        m.remove("layer");
    }
    s.edit(label, |doc: &mut Document, _| {
        let canvas = doc.bounds();
        let selection = doc.selection.clone();
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if let LayerContent::Smart(_) = l.content {
            let sf = SmartFilter { command: cmd.to_string(), params: stored.clone(), blend: photocraft_color::BlendMode::Normal, opacity: 1.0, visible: true };
            return crate::smart_cmds::add_smart_filter(doc, id, sf, selection.as_ref());
        }
        if l.locks.all || l.locks.pixels {
            return Err(EngineError::Other(format!("layer \"{}\" is locked", l.name)));
        }
        // A Background layer gains transparency when the edge mode exposes it (as in Photoshop).
        if float_bg && l.locks.position && l.name == "Background" {
            l.locks.position = false;
            l.locks.transparency = false;
            l.name = "Layer 0".into();
        }
        let LayerContent::Raster(surf) = &mut l.content else {
            return Err(EngineError::Other(format!("{label} needs a pixel layer")));
        };
        let out = f(surf, canvas);
        *surf = match &selection {
            Some(sel) => mix_by_selection(surf, &out, sel),
            None => out,
        };
        Ok(())
    })?;
    Ok(id)
}

fn lens_correction(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let frame = st.doc.bounds();
    let info = camera_info(&st.doc);
    let lc = lens_params(LENS, p, frame, info.as_ref())?;
    let stored = resolved_lens_params(p, &lc);
    let t0 = Stopwatch::start();
    let id =
        run_filter(s, LENS, "Lens Correction", stored.clone(), matches!(lc.edge, EdgeMode::Transparency), &|surf, canvas| lens::correct(surf, canvas, &lc))?;
    Ok(json!({"layer": id.0, "profile": lc.profile.as_ref().map(|p| p.name.clone()), "angle": lc.angle, "scale": lc.scale, "params": stored, "ms": t0.ms()}))
}

fn adaptive_wide_angle(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let frame = st.doc.bounds();
    let info = camera_info(&st.doc);
    let wa = wide_params(WIDE, p, info.as_ref())?;
    let t0 = Stopwatch::start();
    let mesh = wideangle::solve(&wa, frame);
    let cam = wideangle::Camera::new(&wa, frame);
    let mut stored = serde_json::to_value(&wa).unwrap_or(Value::Null);
    if let Some(l) = p.get("layer") {
        stored["layer"] = l.clone();
    }
    let interp = Interp::parse(p.get("interpolation").and_then(Value::as_str).unwrap_or("bicubic"));
    let id = run_filter(s, WIDE, "Adaptive Wide Angle", stored.clone(), true, &|surf, canvas| {
        let tris = mesh.triangles();
        if canvas == frame {
            photocraft_algo::warp::warp_triangles(surf, canvas, &mesh.verts, &tris, interp)
        } else {
            wideangle::apply(surf, canvas, &wa, interp)
        }
    })?;
    let model = match cam.model {
        wideangle::WideModel::Fisheye => "fisheye",
        wideangle::WideModel::Perspective => "perspective",
        wideangle::WideModel::FullSpherical => "fullSpherical",
        wideangle::WideModel::Auto => "auto",
    };
    if let Some(m) = stored.as_object_mut() {
        m.remove("layer");
    }
    Ok(json!({
        "layer": id.0,
        "model": model,
        "focalPx": cam.f,
        "constraints": wa.constraints.len(),
        "curves": mesh.curves,
        "residual": mesh.residual,
        "params": stored,
        "ms": t0.ms(),
    }))
}

fn camera_raw_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cr = raw_params(RAW, p)?;
    let t0 = Stopwatch::start();
    let id = run_filter(s, RAW, "Camera Raw Filter", p.clone(), false, &|surf, canvas| camera_raw_surface(surf, canvas.union(&surf.content_bounds()), &cr))?;
    Ok(json!({"layer": id.0, "identity": cr.is_identity(), "ms": t0.ms()}))
}

fn lens_batch(_s: &mut Session, p: &Value) -> Result<Value> {
    let inputs = crate::file_cmds::batch_inputs(p, LENS_BATCH)?;
    let output = crate::file_cmds::str_param(p, "output", LENS_BATCH)?.to_string();
    let format = p.get("format").and_then(Value::as_str).unwrap_or("same").to_string();
    let mut params = p.clone();
    if let Value::Object(m) = &mut params {
        for k in ["input", "files", "output", "format", "quality"] {
            m.remove(k);
        }
        // Batch default: the EXIF-driven profile, as Photoshop's dialog suggests.
        m.entry("profile").or_insert(json!("auto"));
    }
    let r = crate::file_cmds::process_files(&inputs, &output, &format, crate::file_cmds::f64_param(p, "quality"), "", &|scratch| {
        if scratch.active().is_some_and(|d| d.doc.layers.len() > 1) {
            scratch.execute("layer.flattenImage", json!({}))?;
        }
        scratch.execute(LENS, params.clone())?;
        Ok(())
    });
    Ok(r)
}

const LENS_DOC: &str = r##"{"profile":"none|auto|generic","focalLength":mm=0,"correctDistortion":bool=true,"correctVignette":bool=true,"correctCA":bool=true,"autoScale":bool=false,"distortion":-100..100=0,"redCyan":-100..100=0,"blueYellow":-100..100=0,"vignetteAmount":-100..100=0,"vignetteMidpoint":0..100=50,"vertical":-100..100=0,"horizontal":-100..100=0,"angle":-180..180=0,"scale":50..150=100,"edge":"transparency|edgeExtension|black|white","straighten":[[x,y],[x,y]]?}"##;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: LENS,
            label: "Lens Correction…",
            menu: &["Filter"],
            shortcut: Some("Cmd+Shift+R"),
            params: LENS_DOC,
            enabled: filterable,
            run: lens_correction,
            journal: true,
        },
        CommandSpec {
            id: WIDE,
            label: "Adaptive Wide Angle…",
            menu: &["Filter"],
            shortcut: Some("Cmd+Alt+Shift+A"),
            params: r##"{"model":"auto|fisheye|perspective|fullSpherical","focalLength":0..200=0,"cropFactor":0.1..10=1,"scale":50..150=100,"constraints":[{"a":[x,y],"b":[x,y],"orientation":"free|horizontal|vertical"}]}"##,
            enabled: filterable,
            run: adaptive_wide_angle,
            journal: true,
        },
        CommandSpec {
            id: RAW,
            label: "Camera Raw Filter…",
            menu: &["Filter"],
            shortcut: None,
            params: r##"{"temperature":-100..100=0,"tint":-100..100=0,"exposure":-5..5=0,"contrast":-100..100=0,"highlights":-100..100=0,"shadows":-100..100=0,"whites":-100..100=0,"blacks":-100..100=0,"texture":-100..100=0,"clarity":-100..100=0,"dehaze":-100..100=0,"vibrance":-100..100=0,"saturation":-100..100=0,"curveHighlights":-100..100=0,"curveLights":-100..100=0,"curveDarks":-100..100=0,"curveShadows":-100..100=0,"curveSplits":[25,50,75],"pointCurve":[[in,out]],"pointCurveRed":[[in,out]],"pointCurveGreen":[[in,out]],"pointCurveBlue":[[in,out]],"hslHue":[8],"hslSat":[8],"hslLum":[8],"gradeShadows":{"hue":deg,"sat":0..100,"lum":-100..100},"gradeMidtones":{},"gradeHighlights":{},"gradeGlobal":{},"gradeBlending":0..100=50,"gradeBalance":-100..100=0,"sharpenAmount":0..150=0,"sharpenRadius":0.5..3=1,"sharpenDetail":0..100=25,"sharpenMasking":0..100=0,"noiseLuminance":0..100=0,"noiseLuminanceDetail":0..100=50,"noiseColor":0..100=0,"noiseColorDetail":0..100=50,"grainAmount":0..100=0,"grainSize":0..100=25,"grainRoughness":0..100=50,"vignetteAmount":-100..100=0,"vignetteMidpoint":0..100=50,"vignetteRoundness":-100..100=0,"vignetteFeather":0..100=50,"vignetteHighlights":0..100=0,"vignetteStyle":"highlightPriority|colorPriority|paintOverlay","seed":u32=0}"##,
            enabled: raw_enabled,
            run: camera_raw_cmd,
            journal: true,
        },
        CommandSpec {
            id: LENS_BATCH,
            label: "Lens Correction…",
            menu: &["File", "Automate"],
            shortcut: None,
            params: r##"{"input":folder|[files],"output":folder,"format":"same|jpg|png|tif|psd","quality":0..12?, …Filter › Lens Correction params ("profile" defaults to "auto")}"##,
            enabled: crate::file_cmds::native,
            run: lens_batch,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(depth: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 120, "height": 80, "depth": depth})).unwrap();
        s.execute("layer.new.layer", json!({"name": "grid"})).unwrap();
        s.edit("grid", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            let fmt = surf.format();
            for y in 0..80 {
                for x in 0..120 {
                    let v = if x % 12 < 2 || y % 12 < 2 { 0.9 } else { 0.35 };
                    surf.write_pixel(x, y, &photocraft_raster::from_rgba(&fmt, [v, v * 0.9, v * 0.7, 1.0]));
                }
            }
            Ok(())
        })
        .unwrap();
        s
    }

    fn active_px(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
    }

    #[test]
    fn lens_correction_runs_undoes_and_resolves_profiles() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            assert!(s.is_enabled(LENS));
            let before = active_px(&s, 1, 1);
            let r = s.execute(LENS, json!({"distortion": -40, "vignetteAmount": -50})).unwrap();
            assert!(r["ms"].as_f64().is_some());
            let after = active_px(&s, 1, 1);
            assert!(after[3] < 0.5, "depth {depth}: corner cleared by transparency edge: {after:?}");
            s.undo();
            assert_eq!(active_px(&s, 1, 1), before);
        }
        // EXIF-driven auto profile.
        let mut s = session(8);
        s.edit("exif", |doc, _| {
            doc.metadata.exif =
                Some(std::sync::Arc::new(exif::build(&exif::CameraInfo { focal_length: Some(16.0), focal_length_35mm: Some(24.0), ..Default::default() })));
            Ok(())
        })
        .unwrap();
        let r = s.execute(LENS, json!({"profile": "auto", "autoScale": true, "edge": "edgeExtension"})).unwrap();
        assert_eq!(r["params"]["profile"], "generic");
        assert_eq!(r["params"]["focalLength"], 24.0);
        assert!(r["scale"].as_f64().unwrap() >= 100.0);
        let r = s.execute(LENS, json!({"straighten": [[0, 0], [100, 5]]})).unwrap();
        assert!((r["angle"].as_f64().unwrap() + 2.862).abs() < 0.01, "{r}");
        assert!(s.execute(LENS, json!({"edge": "rainbow"})).is_err());
        assert!(s.execute(LENS, json!({"profile": "zeiss"})).is_err());
    }

    #[test]
    fn lens_correction_as_smart_filter() {
        let mut s = session(8);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        let r = s.execute(LENS, json!({"vignetteAmount": 80, "edge": "edgeExtension"})).unwrap();
        let d = s.active().unwrap();
        let l = d.doc.layer(LayerId(r["layer"].as_u64().unwrap())).unwrap();
        let LayerContent::Smart(sm) = &l.content else { panic!("smart") };
        assert_eq!(sm.smart_filters.len(), 1);
        assert_eq!(sm.smart_filters[0].command, LENS);
        let corner = active_px(&s, 3, 3);
        assert!(corner[0] > 0.4, "{corner:?}");
    }

    #[test]
    fn adaptive_wide_angle_straightens() {
        let mut s = session(8);
        let r = s
            .execute(WIDE, json!({"model": "fisheye", "focalLength": 10, "constraints": [{"a": [10, 15], "b": [110, 15], "orientation": "horizontal"}]}))
            .unwrap();
        assert_eq!(r["model"], "fisheye");
        assert!(r["residual"].as_f64().unwrap() < 2.0, "{r}");
        assert!(s.execute(WIDE, json!({"constraints": [{"a": [0, 0]}]})).is_err());
        // Smart filter re-render.
        let mut s = session(16);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        s.execute(WIDE, json!({"model": "perspective", "scale": 90})).unwrap();
        assert!(active_px(&s, 1, 1)[3] < 0.5);
    }

    #[test]
    fn camera_raw_filter_on_layers_selection_and_smart_objects() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            assert!(s.is_enabled(RAW));
            let before = active_px(&s, 60, 40);
            s.execute(RAW, json!({"exposure": 1.0, "vibrance": 30, "clarity": 20})).unwrap();
            let after = active_px(&s, 60, 40);
            assert!(after[1] > before[1] + 0.05, "depth {depth}: {before:?} → {after:?}");
            s.undo();
        }
        // Selection limits it.
        let mut s = session(8);
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 60, "height": 80})).unwrap();
        let inside_before = active_px(&s, 10, 40);
        let outside_before = active_px(&s, 100, 40);
        s.execute(RAW, json!({"exposure": -1.0})).unwrap();
        assert!(active_px(&s, 10, 40)[1] < inside_before[1]);
        assert_eq!(active_px(&s, 100, 40), outside_before);
        assert!(s.execute(RAW, json!({"exposure": "bright"})).is_err());
        // Smart filter.
        let mut s = session(8);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        s.execute(RAW, json!({"saturation": -100})).unwrap();
        let p = active_px(&s, 60, 40);
        assert!((p[0] - p[2]).abs() < 0.01, "{p:?}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn lens_correction_batch_over_files() {
        let dir = std::env::temp_dir().join(format!("pc-lens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in")).unwrap();
        let mut s = session(8);
        s.execute("layer.flattenImage", json!({})).unwrap();
        for k in 0..2 {
            let path = dir.join("in").join(format!("shot{k}.png"));
            crate::file_cmds::save_doc(&s.active().unwrap().doc, path.to_str().unwrap(), None).unwrap();
        }
        let r = s
            .execute(
                LENS_BATCH,
                json!({"input": dir.join("in").to_str().unwrap(), "output": dir.join("out").to_str().unwrap(), "vignetteAmount": 60, "edge": "edgeExtension"}),
            )
            .unwrap();
        assert_eq!(r["files"].as_array().unwrap().len(), 2, "{r}");
        assert!(r["errors"].as_array().unwrap().is_empty());
        assert!(dir.join("out").join("shot0.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
