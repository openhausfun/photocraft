//! Window › Clone Source: five clone source slots for Clone Stamp and Healing Brush, each with
//! a sample point, an offset, W/H scale, rotation and flips, plus the overlay options.
//!
//! `paint.cloneStamp` / `paint.healingBrush` read the active slot when the call names no
//! `source`/`offset`, and always take its transform unless the call gives its own
//! (`scale`, `rotation`, `flipH`, `flipV`). The transform maps the destination around the
//! anchor (where the stroke started after the source was set) onto the source around the
//! sample point: a 200 % width paints the source twice as wide, a 30° rotation paints it turned
//! 30° counter-clockwise on screen (Photoshop's sense).

use photocraft_geom::Rect;
use photocraft_paint::retouch::Region;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{always, bad};
use crate::commands::CommandSpec;
use crate::{Result, Session};

pub const SLOTS: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloneSource {
    /// Sample point (document px) set by ⌥-click.
    pub source: Option<[f64; 2]>,
    /// Destination point paired with `source` (first aligned stroke after setting it).
    pub anchor: Option<[f64; 2]>,
    /// Layer the source was set on (informational, like the panel's document/layer line).
    pub layer: Option<u64>,
    /// W and H in percent.
    pub scale: [f64; 2],
    /// Degrees, counter-clockwise.
    pub rotation: f64,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Default for CloneSource {
    fn default() -> Self {
        CloneSource { source: None, anchor: None, layer: None, scale: [100.0, 100.0], rotation: 0.0, flip_h: false, flip_v: false }
    }
}

impl CloneSource {
    pub fn is_identity(&self) -> bool {
        (self.scale[0] - 100.0).abs() < 1e-9
            && (self.scale[1] - 100.0).abs() < 1e-9
            && self.rotation.rem_euclid(360.0).abs() < 1e-9
            && !self.flip_h
            && !self.flip_v
    }
    /// The panel's Offset X/Y: source − anchor (once a stroke paired them).
    pub fn offset(&self) -> Option<[f64; 2]> {
        Some([self.source?[0] - self.anchor?[0], self.source?[1] - self.anchor?[1]])
    }
    fn to_json(&self, i: usize) -> Value {
        json!({
            "index": i, "source": self.source, "anchor": self.anchor, "offset": self.offset(), "layer": self.layer,
            "width": self.scale[0], "height": self.scale[1], "rotation": self.rotation, "flipH": self.flip_h, "flipV": self.flip_v,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloneOverlay {
    pub show: bool,
    /// Percent.
    pub opacity: f32,
    /// Clipped to the brush.
    pub clipped: bool,
    pub auto_hide: bool,
    pub invert: bool,
    /// normal | darken | lighten | difference
    pub blend: String,
}

impl Default for CloneOverlay {
    fn default() -> Self {
        CloneOverlay { show: true, opacity: 100.0, clipped: true, auto_hide: true, invert: false, blend: "normal".into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CloneSources {
    pub slots: [CloneSource; SLOTS],
    pub active: usize,
    pub overlay: CloneOverlay,
}

impl CloneSources {
    pub fn active(&self) -> &CloneSource {
        &self.slots[self.active.min(SLOTS - 1)]
    }
    pub fn active_mut(&mut self) -> &mut CloneSource {
        &mut self.slots[self.active.min(SLOTS - 1)]
    }
}

/// How a clone stroke samples: destination `d` reads the source at
/// `source + m · (d − anchor)` (`m` row-major 2 × 2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mapping {
    pub source: (f64, f64),
    pub anchor: (f64, f64),
    pub m: [f64; 4],
}

impl Mapping {
    pub fn is_translation(&self) -> bool {
        (self.m[0] - 1.0).abs() < 1e-12 && self.m[1].abs() < 1e-12 && self.m[2].abs() < 1e-12 && (self.m[3] - 1.0).abs() < 1e-12
    }
    /// Integer translation (identity transform).
    pub fn offset(&self) -> (i32, i32) {
        ((self.source.0 - self.anchor.0).round() as i32, (self.source.1 - self.anchor.1).round() as i32)
    }
    pub fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let (dx, dy) = (x - self.anchor.0, y - self.anchor.1);
        (self.source.0 + self.m[0] * dx + self.m[1] * dy, self.source.1 + self.m[2] * dx + self.m[3] * dy)
    }
    /// Source pixels needed to resample `dst` (with a one-pixel apron for bilinear).
    pub fn source_rect(&self, dst: Rect) -> Rect {
        let pts = [(dst.x0, dst.y0), (dst.x1, dst.y0), (dst.x0, dst.y1), (dst.x1, dst.y1)].map(|(x, y)| self.map(x as f64, y as f64));
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in pts {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        Rect::new(x0.floor() as i32 - 2, y0.floor() as i32 - 2, x1.ceil() as i32 + 2, y1.ceil() as i32 + 2)
    }
}

/// Matrix taking destination offsets to source offsets for a slot's W/H, rotation and flips.
pub fn transform_matrix(scale: [f64; 2], rotation_deg: f64, flip_h: bool, flip_v: bool) -> [f64; 4] {
    let sx = (scale[0] / 100.0).max(1e-3) * if flip_h { -1.0 } else { 1.0 };
    let sy = (scale[1] / 100.0).max(1e-3) * if flip_v { -1.0 } else { 1.0 };
    // Forward (source → destination): rotate after scaling. Rotation is counter-clockwise on
    // screen, which is clockwise in y-down maths.
    let (s, c) = (-rotation_deg.to_radians()).sin_cos();
    let f = [c * sx, -s * sy, s * sx, c * sy];
    let det = f[0] * f[3] - f[1] * f[2];
    [f[3] / det, -f[1] / det, -f[2] / det, f[0] / det]
}

fn pt(p: &Value, k: &str) -> Option<(f64, f64)> {
    let a = p.get(k)?.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

/// Resolve a clone stroke's mapping from its params and the active slot. `first` is the
/// stroke's first point. With `"aligned"` strokes the slot remembers the anchor so later
/// strokes keep sampling relative to the original pairing.
pub fn mapping(s: &mut Session, p: &Value, first: (f64, f64), cmd: &str) -> Result<Mapping> {
    let aligned = p.get("aligned").and_then(Value::as_bool).unwrap_or(true);
    let slot = s.presets.clone.active().clone();
    let explicit_tf = ["scale", "rotation", "flipH", "flipV"].iter().any(|k| p.get(*k).is_some());
    let m = if explicit_tf {
        let sc = p
            .get("scale")
            .and_then(Value::as_array)
            .map_or([100.0, 100.0], |a| [a.first().and_then(Value::as_f64).unwrap_or(100.0), a.get(1).and_then(Value::as_f64).unwrap_or(100.0)]);
        let flag = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
        transform_matrix(sc, p.get("rotation").and_then(Value::as_f64).unwrap_or(0.0), flag("flipH"), flag("flipV"))
    } else {
        transform_matrix(slot.scale, slot.rotation, slot.flip_h, slot.flip_v)
    };
    let anchor_param = pt(p, "anchor");
    if let Some(off) = pt(p, "offset") {
        let a = anchor_param.unwrap_or(first);
        return Ok(Mapping { source: (a.0 + off.0, a.1 + off.1), anchor: a, m });
    }
    if let Some(src) = pt(p, "source") {
        return Ok(Mapping { source: src, anchor: anchor_param.unwrap_or(first), m });
    }
    let Some(src) = slot.source else { return Err(bad(cmd, "missing `source` ([x,y]) or `offset` ([dx,dy]); or set one with cloneSource.set")) };
    let src = (src[0], src[1]);
    let anchor = match (aligned, slot.anchor) {
        (true, Some(a)) => (a[0], a[1]),
        _ => first,
    };
    if aligned {
        s.presets.clone.active_mut().anchor = Some([anchor.0, anchor.1]);
    }
    Ok(Mapping { source: src, anchor, m })
}

/// Bilinear resample of `src` (covering [`Mapping::source_rect`]) onto `dst`, interpolating
/// premultiplied by the alpha channel `alpha` when there is one. Outside `src` is transparent.
pub fn resample(src: &Region, dst: Rect, map: &Mapping, alpha: Option<usize>) -> Region {
    let n = src.ch;
    let mut out = Region::new(dst, n);
    let w = dst.width() as usize;
    let fetch = |x: i32, y: i32, buf: &mut [f32]| {
        if x < src.rect.x0 || y < src.rect.y0 || x >= src.rect.x1 || y >= src.rect.y1 {
            buf.fill(0.0);
            return;
        }
        buf.copy_from_slice(src.px(x, y));
        if let Some(a) = alpha {
            let av = buf[a];
            for (c, v) in buf.iter_mut().enumerate() {
                if c != a {
                    *v *= av;
                }
            }
        }
    };
    let mut q = [vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]];
    for (i, px) in out.data.chunks_exact_mut(n).enumerate() {
        let (x, y) = (dst.x0 + (i % w) as i32, dst.y0 + (i / w) as i32);
        let (sx, sy) = map.map(x as f64 + 0.5, y as f64 + 0.5);
        let (fx, fy) = (sx - 0.5, sy - 0.5);
        let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
        let (tx, ty) = ((fx - x0 as f64) as f32, (fy - y0 as f64) as f32);
        fetch(x0, y0, &mut q[0]);
        fetch(x0 + 1, y0, &mut q[1]);
        fetch(x0, y0 + 1, &mut q[2]);
        fetch(x0 + 1, y0 + 1, &mut q[3]);
        for c in 0..n {
            let top = q[0][c] + (q[1][c] - q[0][c]) * tx;
            let bot = q[2][c] + (q[3][c] - q[2][c]) * tx;
            px[c] = top + (bot - top) * ty;
        }
        if let Some(a) = alpha {
            let av = px[a];
            for (c, v) in px.iter_mut().enumerate() {
                if c != a {
                    *v = if av > 1e-6 { *v / av } else { 0.0 };
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ commands

fn index(s: &Session, p: &Value, cmd: &str) -> Result<usize> {
    match p.get("index").and_then(Value::as_u64) {
        Some(i) if (i as usize) < SLOTS => Ok(i as usize),
        Some(i) => Err(bad(cmd, format!("index {i} out of range (0..{SLOTS})"))),
        None => Ok(s.presets.clone.active),
    }
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let c = &s.presets.clone;
    Ok(
        json!({"active": c.active, "sources": c.slots.iter().enumerate().map(|(i, x)| x.to_json(i)).collect::<Vec<_>>(), "overlay": serde_json::to_value(&c.overlay).unwrap_or(Value::Null)}),
    )
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "cloneSource.select";
    let i = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "missing `index`"))? as usize;
    if i >= SLOTS {
        return Err(bad(CMD, format!("index {i} out of range (0..{SLOTS})")));
    }
    s.presets.clone.active = i;
    Ok(s.presets.clone.active().to_json(i))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "cloneSource.set";
    let i = index(s, p, CMD)?;
    let active_layer = s.active().and_then(|d| d.active_layer).map(|l| l.0);
    let slot = &mut s.presets.clone.slots[i];
    if let Some((x, y)) = pt(p, "source") {
        // A new sample point re-pairs with the next stroke.
        slot.source = Some([x, y]);
        slot.anchor = None;
        slot.layer = p.get("layer").and_then(Value::as_u64).or(active_layer);
    }
    if let Some((dx, dy)) = pt(p, "offset") {
        // Offset X/Y fields: keep the anchor (or the source as anchor) and move the source.
        let a = slot.anchor.or(slot.source).ok_or_else(|| bad(CMD, "set a `source` before an offset"))?;
        slot.anchor = Some(a);
        slot.source = Some([a[0] + dx, a[1] + dy]);
    }
    if let Some(a) = p.get("scale").and_then(Value::as_array) {
        slot.scale = [a.first().and_then(Value::as_f64).unwrap_or(100.0), a.get(1).and_then(Value::as_f64).unwrap_or(100.0)];
    }
    let num = |k: &str| p.get(k).and_then(Value::as_f64);
    if let Some(w) = num("width") {
        slot.scale[0] = w;
    }
    if let Some(h) = num("height") {
        slot.scale[1] = h;
    }
    if slot.scale.iter().any(|v| !(1.0..=1000.0).contains(&v.abs())) {
        slot.scale = slot.scale.map(|v| v.clamp(1.0, 1000.0));
    }
    if let Some(r) = num("rotation") {
        slot.rotation = (r + 180.0).rem_euclid(360.0) - 180.0;
    }
    if let Some(b) = p.get("flipH").and_then(Value::as_bool) {
        slot.flip_h = b;
    }
    if let Some(b) = p.get("flipV").and_then(Value::as_bool) {
        slot.flip_v = b;
    }
    if p.get("clear").and_then(Value::as_bool) == Some(true) {
        *slot = CloneSource::default();
    }
    if p.get("select").and_then(Value::as_bool).unwrap_or(true) {
        s.presets.clone.active = i;
    }
    Ok(s.presets.clone.slots[i].to_json(i))
}

fn reset_transform(s: &mut Session, p: &Value) -> Result<Value> {
    let i = index(s, p, "cloneSource.resetTransform")?;
    let slot = &mut s.presets.clone.slots[i];
    (slot.scale, slot.rotation, slot.flip_h, slot.flip_v) = ([100.0, 100.0], 0.0, false, false);
    Ok(slot.to_json(i))
}

fn overlay(s: &mut Session, p: &Value) -> Result<Value> {
    let o = &mut s.presets.clone.overlay;
    let flag = |k: &str| p.get(k).and_then(Value::as_bool);
    if let Some(b) = flag("show") {
        o.show = b;
    }
    if let Some(v) = p.get("opacity").and_then(Value::as_f64) {
        o.opacity = (v as f32).clamp(0.0, 100.0);
    }
    if let Some(b) = flag("clipped") {
        o.clipped = b;
    }
    if let Some(b) = flag("autoHide") {
        o.auto_hide = b;
    }
    if let Some(b) = flag("invert") {
        o.invert = b;
    }
    if let Some(m) = p.get("blend").and_then(Value::as_str) {
        if !matches!(m, "normal" | "darken" | "lighten" | "difference") {
            return Err(bad("cloneSource.overlay", format!("unknown overlay mode `{m}` (normal|darken|lighten|difference)")));
        }
        o.blend = m.into();
    }
    Ok(serde_json::to_value(&*o).unwrap_or(Value::Null))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "cloneSource.list",
            label: "Clone Sources",
            menu: &[],
            shortcut: None,
            params: "{} → {active,sources:[{index,source,anchor,offset,layer,width,height,rotation,flipH,flipV}],overlay}",
            enabled: always,
            run: list,
            journal: false,
        },
        CommandSpec {
            id: "cloneSource.select",
            label: "Select Clone Source",
            menu: &[],
            shortcut: None,
            params: r##"{"index":0..4}"##,
            enabled: always,
            run: select,
            journal: true,
        },
        CommandSpec {
            id: "cloneSource.set",
            label: "Set Clone Source",
            menu: &[],
            shortcut: None,
            params: r##"{"index":0..4?=active,"source":[x,y]? (⌥-click; re-pairs with the next stroke),"offset":[dx,dy]?,"width":%?,"height":%?|"scale":[w%,h%]?,"rotation":deg?,"flipH":bool?,"flipV":bool?,"layer":id?,"clear":bool?,"select":bool=true}"##,
            enabled: always,
            run: set,
            journal: true,
        },
        CommandSpec {
            id: "cloneSource.resetTransform",
            label: "Reset Transform",
            menu: &[],
            shortcut: None,
            params: r##"{"index":0..4?=active}"##,
            enabled: always,
            run: reset_transform,
            journal: true,
        },
        CommandSpec {
            id: "cloneSource.overlay",
            label: "Clone Source Overlay",
            menu: &[],
            shortcut: None,
            params: r##"{"show":bool?,"opacity":0..100?,"clipped":bool?,"autoHide":bool?,"invert":bool?,"blend":"normal|darken|lighten|difference"?}"##,
            enabled: always,
            run: overlay,
            journal: true,
        },
    ]
}
