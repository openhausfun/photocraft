//! Warp geometry shared by Edit › Transform › Warp, smart-object warps and Warp Text.
//!
//! Two kinds of warp, both forward maps from a source box to document space:
//! - **Styles** ([`WarpStyle`], Photoshop's fifteen presets plus bend and horizontal/vertical
//!   distortion, the PSD `warp` descriptor's `warpStyle`/`warpValue`/`warpPerspective*`). The
//!   shapes are our own closed-form approximations of each style's look (observed behaviour, not
//!   Adobe's formulas). Every style is the identity at bend 0 and continuous in the bend.
//! - **Custom meshes** ([`BezierMesh`]): a grid of bicubic Bezier patches. The default is one
//!   patch (a 4×4 control grid); split warps add patch rows/columns by exact de Casteljau
//!   subdivision, so splitting never changes the shape.
//!
//! Pure geometry: resampling lives in `photocraft-algo::warp`.

use serde::{Deserialize, Serialize};

/// Warp style: none, a custom mesh, or one of Photoshop's presets (menu order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WarpStyle {
    #[default]
    None,
    Custom,
    Arc,
    ArcLower,
    ArcUpper,
    Arch,
    Bulge,
    ShellLower,
    ShellUpper,
    Flag,
    Wave,
    Fish,
    Rise,
    Fisheye,
    Inflate,
    Squeeze,
    Twist,
}

impl WarpStyle {
    /// The fifteen presets in Photoshop's menu order.
    pub const PRESETS: [WarpStyle; 15] = [
        WarpStyle::Arc,
        WarpStyle::ArcLower,
        WarpStyle::ArcUpper,
        WarpStyle::Arch,
        WarpStyle::Bulge,
        WarpStyle::ShellLower,
        WarpStyle::ShellUpper,
        WarpStyle::Flag,
        WarpStyle::Wave,
        WarpStyle::Fish,
        WarpStyle::Rise,
        WarpStyle::Fisheye,
        WarpStyle::Inflate,
        WarpStyle::Squeeze,
        WarpStyle::Twist,
    ];

    /// Every style (none, custom, presets).
    pub fn all() -> impl Iterator<Item = WarpStyle> {
        [WarpStyle::None, WarpStyle::Custom].into_iter().chain(Self::PRESETS)
    }

    /// PSD `warpStyle` enum value (`warpArc`, `warpCustom`, `warpNone`…).
    pub fn psd_name(self) -> &'static str {
        match self {
            WarpStyle::None => "warpNone",
            WarpStyle::Custom => "warpCustom",
            WarpStyle::Arc => "warpArc",
            WarpStyle::ArcLower => "warpArcLower",
            WarpStyle::ArcUpper => "warpArcUpper",
            WarpStyle::Arch => "warpArch",
            WarpStyle::Bulge => "warpBulge",
            WarpStyle::ShellLower => "warpShellLower",
            WarpStyle::ShellUpper => "warpShellUpper",
            WarpStyle::Flag => "warpFlag",
            WarpStyle::Wave => "warpWave",
            WarpStyle::Fish => "warpFish",
            WarpStyle::Rise => "warpRise",
            WarpStyle::Fisheye => "warpFisheye",
            WarpStyle::Inflate => "warpInflate",
            WarpStyle::Squeeze => "warpSqueeze",
            WarpStyle::Twist => "warpTwist",
        }
    }

    /// Short id used by commands (`arc`, `arcLower`, `custom`, `none`).
    pub fn id(self) -> &'static str {
        let p = self.psd_name().trim_start_matches("warp");
        // Lower-case first letter without allocating: every id is listed.
        match p {
            "None" => "none",
            "Custom" => "custom",
            "Arc" => "arc",
            "ArcLower" => "arcLower",
            "ArcUpper" => "arcUpper",
            "Arch" => "arch",
            "Bulge" => "bulge",
            "ShellLower" => "shellLower",
            "ShellUpper" => "shellUpper",
            "Flag" => "flag",
            "Wave" => "wave",
            "Fish" => "fish",
            "Rise" => "rise",
            "Fisheye" => "fisheye",
            "Inflate" => "inflate",
            "Squeeze" => "squeeze",
            _ => "twist",
        }
    }

    /// Menu label (`Arc Lower`).
    pub fn label(self) -> &'static str {
        match self {
            WarpStyle::None => "None",
            WarpStyle::Custom => "Custom",
            WarpStyle::Arc => "Arc",
            WarpStyle::ArcLower => "Arc Lower",
            WarpStyle::ArcUpper => "Arc Upper",
            WarpStyle::Arch => "Arch",
            WarpStyle::Bulge => "Bulge",
            WarpStyle::ShellLower => "Shell Lower",
            WarpStyle::ShellUpper => "Shell Upper",
            WarpStyle::Flag => "Flag",
            WarpStyle::Wave => "Wave",
            WarpStyle::Fish => "Fish",
            WarpStyle::Rise => "Rise",
            WarpStyle::Fisheye => "Fisheye",
            WarpStyle::Inflate => "Inflate",
            WarpStyle::Squeeze => "Squeeze",
            WarpStyle::Twist => "Twist",
        }
    }

    /// Parses a short id, a PSD name or a label (case-insensitive, spaces ignored).
    pub fn parse(s: &str) -> Option<WarpStyle> {
        let k: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        Self::all().find(|w| w.id().to_ascii_lowercase() == k || w.psd_name().to_ascii_lowercase() == k)
    }

    pub fn is_preset(self) -> bool {
        !matches!(self, WarpStyle::None | WarpStyle::Custom)
    }
}

/// A preset style prepared for one box: strengths and the box it bends.
#[derive(Clone, Copy, Debug)]
pub struct StyleWarp {
    style: WarpStyle,
    /// Bend, -1..1.
    b: f64,
    /// Horizontal / vertical distortion, -1..1.
    hd: f64,
    vd: f64,
    /// Warp along the vertical axis (Photoshop's "Vertical" orientation).
    vertical: bool,
    c: (f64, f64),
    h: (f64, f64),
}

impl StyleWarp {
    /// Prepares `style` with bend / distortions in percent (-100..100) for `[x0, y0, x1, y1]`.
    /// `None` for non-presets, an all-zero warp or an empty box.
    pub fn new(style: WarpStyle, bend: f64, h_distort: f64, v_distort: f64, vertical: bool, bounds: [f64; 4]) -> Option<StyleWarp> {
        if !style.is_preset() {
            return None;
        }
        let [x0, y0, x1, y1] = bounds;
        let (hw, hh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        if !(hw > 0.0 && hh > 0.0) {
            return None;
        }
        let pct = |v: f64| if v.is_finite() { (v / 100.0).clamp(-1.0, 1.0) } else { 0.0 };
        let (b, hd, vd) = (pct(bend), pct(h_distort), pct(v_distort));
        if b == 0.0 && hd == 0.0 && vd == 0.0 {
            return None;
        }
        Some(StyleWarp { style, b, hd, vd, vertical, c: ((x0 + x1) / 2.0, (y0 + y1) / 2.0), h: (hw, hh) })
    }

    /// Longest segment (px) worth sending through [`StyleWarp::apply`] unsplit.
    pub fn max_segment(&self) -> f64 {
        (self.h.0.min(self.h.1) / 16.0).max(0.5)
    }

    /// Maps a point.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        // Normalised coordinates: u along the warp axis, v across it, both -1..1 over the box.
        let (dx, dy) = (x - self.c.0, y - self.c.1);
        let (u, v, hu, hv) =
            if self.vertical { (dy / self.h.1, dx / self.h.0, self.h.1, self.h.0) } else { (dx / self.h.0, dy / self.h.1, self.h.0, self.h.1) };
        let (mut u2, mut v2) = self.bend(u, v, hu / hv);
        // Distortions: a perspective-like taper along each axis.
        if self.hd != 0.0 {
            v2 *= (1.0 + self.hd * u2).max(0.05);
        }
        if self.vd != 0.0 {
            u2 *= (1.0 + self.vd * v2).max(0.05);
        }
        let (ox, oy) = if self.vertical { (v2 * self.h.0, u2 * self.h.1) } else { (u2 * self.h.0, v2 * self.h.1) };
        (self.c.0 + ox, self.c.1 + oy)
    }

    /// The style's mapping in normalised space; `aspect` = half-length along / across the axis.
    fn bend(&self, u: f64, v: f64, aspect: f64) -> (f64, f64) {
        use std::f64::consts::{FRAC_PI_2, PI};
        let b = self.b;
        let bell = (1.0 - u * u).max(0.0);
        match self.style {
            WarpStyle::Arc => arc(u, v, b, aspect),
            WarpStyle::ArcLower => lerp2((u, v), arc(u, v, b, aspect), (v + 1.0) / 2.0),
            WarpStyle::ArcUpper => lerp2((u, v), arc(u, v, b, aspect), (1.0 - v) / 2.0),
            // Every column moves up (b > 0) by the same parabola; verticals stay vertical.
            WarpStyle::Arch => (u, v - b * bell),
            WarpStyle::Bulge => (u, v * (1.0 + b * bell)),
            // The bottom (top) edge bows out while the opposite edge stays straight.
            WarpStyle::ShellLower => (u, v + b * bell * (v + 1.0) / 2.0),
            WarpStyle::ShellUpper => (u, v - b * bell * (1.0 - v) / 2.0),
            WarpStyle::Flag => (u, v - b * 0.5 * (PI * u).sin()),
            WarpStyle::Wave => (u, v - b * 0.5 * (PI * u + v * FRAC_PI_2).sin()),
            // A body swelling towards the head (left) and a narrow tail, curving up.
            WarpStyle::Fish => (u, v * (1.0 + b * 0.6 * (1.0 - u) * bell.sqrt()) - b * 0.35 * bell),
            WarpStyle::Rise => (u, v - b * 0.5 * (FRAC_PI_2 * u).sin()),
            WarpStyle::Fisheye => {
                let r2 = (u * u + v * v).min(1.0);
                let k = 1.0 + b * (1.0 - r2);
                (u * k, v * k)
            }
            WarpStyle::Inflate => {
                let (bu, bv) = ((1.0 - v * v).max(0.0), bell);
                (u * (1.0 + b * 0.5 * bu), v * (1.0 + b * 0.5 * bv))
            }
            WarpStyle::Squeeze => (u * (1.0 + b * 0.5 * (1.0 - v * v).max(0.0)), v * (1.0 - b * 0.5 * bell)),
            WarpStyle::Twist => {
                let r = (u * u + v * v).sqrt();
                if r >= 1.0 {
                    return (u, v);
                }
                // Rotate in an isotropic frame so the twist stays circular on wide boxes.
                let a = b * FRAC_PI_2 * (1.0 - r) * (1.0 - r);
                let (s, c) = a.sin_cos();
                let (x, y) = (u * aspect, v);
                ((x * c - y * s) / aspect, x * s + y * c)
            }
            WarpStyle::None | WarpStyle::Custom => (u, v),
        }
    }
}

/// Along a circular arc: the box centre line becomes an arc spanning `b × 180°`.
fn arc(u: f64, v: f64, b: f64, aspect: f64) -> (f64, f64) {
    if b.abs() < 1e-6 {
        return (u, v);
    }
    // Work in units of the half-height so the circle is round; arc length of the centre line
    // equals the box half-length at the ends.
    let half = aspect;
    let theta_max = b * std::f64::consts::FRAC_PI_2;
    let r = half / theta_max;
    let theta = u * theta_max;
    let rr = r - v;
    let x = rr * theta.sin();
    let y = r - rr * theta.cos();
    (x / half, y)
}

fn lerp2(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    let t = t.clamp(0.0, 1.0);
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn bernstein(t: f64) -> [f64; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t]
}

fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// De Casteljau split of a cubic at `t`: 7 points (left 0..=3, right 3..=6).
fn split_cubic(p: [[f64; 2]; 4], t: f64) -> [[f64; 2]; 7] {
    let a = lerp(p[0], p[1], t);
    let b = lerp(p[1], p[2], t);
    let c = lerp(p[2], p[3], t);
    let d = lerp(a, b, t);
    let e = lerp(b, c, t);
    let f = lerp(d, e, t);
    [p[0], a, d, f, e, c, p[3]]
}

/// Inverse of the Bernstein interpolation matrix at t = 0, 1/3, 2/3, 1 (rows: control point,
/// columns: sample). Exact rational values.
const BERN_INV: [[f64; 4]; 4] = [[1.0, 0.0, 0.0, 0.0], [-5.0 / 6.0, 3.0, -1.5, 1.0 / 3.0], [1.0 / 3.0, -1.5, 3.0, -5.0 / 6.0], [0.0, 0.0, 0.0, 1.0]];

/// A grid of bicubic Bezier patches over the unit square of a source box.
///
/// `us`/`vs` are the patch boundaries in the box's normalised coordinates (`0..=1`, increasing,
/// first 0 and last 1). Control points are a `(3·(us.len()-1)+1) × (3·(vs.len()-1)+1)` grid in
/// row-major order, in output (document) coordinates; neighbouring patches share their edge row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BezierMesh {
    pub us: Vec<f64>,
    pub vs: Vec<f64>,
    pub points: Vec<[f64; 2]>,
}

impl BezierMesh {
    /// The identity mesh over `bounds` with `cols × rows` patches (Photoshop's default is 1 × 1,
    /// a 4 × 4 control grid).
    pub fn identity(bounds: [f64; 4], cols: usize, rows: usize) -> BezierMesh {
        let (cols, rows) = (cols.clamp(1, 64), rows.clamp(1, 64));
        let us: Vec<f64> = (0..=cols).map(|i| i as f64 / cols as f64).collect();
        let vs: Vec<f64> = (0..=rows).map(|i| i as f64 / rows as f64).collect();
        Self::fit(&|s, t| [bounds[0] + s * (bounds[2] - bounds[0]), bounds[1] + t * (bounds[3] - bounds[1])], us, vs)
    }

    /// Fits a mesh with patch boundaries `us`/`vs` to the map `f` (normalised → output) by
    /// interpolating it at the patch's thirds. Exact for bicubic maps (affine maps included).
    pub fn fit(f: &dyn Fn(f64, f64) -> [f64; 2], us: Vec<f64>, vs: Vec<f64>) -> BezierMesh {
        let (pc, pr) = (us.len() - 1, vs.len() - 1);
        let (nx, ny) = (3 * pc + 1, 3 * pr + 1);
        let mut points = vec![[0.0; 2]; nx * ny];
        for pj in 0..pr {
            for pi in 0..pc {
                let mut s = [[[0.0; 2]; 4]; 4]; // s[row j][col i]
                for (j, row) in s.iter_mut().enumerate() {
                    for (i, v) in row.iter_mut().enumerate() {
                        let u = us[pi] + (us[pi + 1] - us[pi]) * i as f64 / 3.0;
                        let t = vs[pj] + (vs[pj + 1] - vs[pj]) * j as f64 / 3.0;
                        *v = f(u, t);
                    }
                }
                for j in 0..4 {
                    for i in 0..4 {
                        let mut acc = [0.0; 2];
                        for (a, wa) in BERN_INV[j].iter().enumerate() {
                            for (b, wb) in BERN_INV[i].iter().enumerate() {
                                let k = wa * wb;
                                acc[0] += k * s[a][b][0];
                                acc[1] += k * s[a][b][1];
                            }
                        }
                        points[(3 * pj + j) * nx + 3 * pi + i] = acc;
                    }
                }
            }
        }
        BezierMesh { us, vs, points }
    }

    /// Control points per row.
    pub fn nx(&self) -> usize {
        3 * (self.us.len() - 1) + 1
    }
    /// Control point rows.
    pub fn ny(&self) -> usize {
        3 * (self.vs.len() - 1) + 1
    }
    pub fn point(&self, i: usize, j: usize) -> [f64; 2] {
        self.points[j * self.nx() + i]
    }

    /// Checks the invariants (knots and point count); malformed data from files is rejected.
    pub fn is_valid(&self) -> bool {
        let knots_ok = |k: &[f64]| k.len() >= 2 && k[0] == 0.0 && k[k.len() - 1] == 1.0 && k.windows(2).all(|w| w[1] > w[0]);
        knots_ok(&self.us)
            && knots_ok(&self.vs)
            && self.points.len() == self.nx() * self.ny()
            && self.points.iter().all(|p| p[0].is_finite() && p[1].is_finite())
    }

    fn locate(knots: &[f64], s: f64) -> (usize, f64) {
        let n = knots.len() - 1;
        let s = s.clamp(0.0, 1.0);
        let mut i = 0;
        while i + 1 < n && s > knots[i + 1] {
            i += 1;
        }
        let w = knots[i + 1] - knots[i];
        (i, if w > 0.0 { (s - knots[i]) / w } else { 0.0 })
    }

    /// Evaluates the surface at normalised `(s, t)`.
    pub fn eval(&self, s: f64, t: f64) -> [f64; 2] {
        let (pi, a) = Self::locate(&self.us, s);
        let (pj, b) = Self::locate(&self.vs, t);
        let (ba, bb) = (bernstein(a), bernstein(b));
        let nx = self.nx();
        let mut out = [0.0; 2];
        for (j, wb) in bb.iter().enumerate() {
            for (i, wa) in ba.iter().enumerate() {
                let p = self.points[(3 * pj + j) * nx + 3 * pi + i];
                let k = wa * wb;
                out[0] += p[0] * k;
                out[1] += p[1] * k;
            }
        }
        out
    }

    /// Splits the patch column containing `s` at `s` (a vertical split line). Exact: the surface
    /// is unchanged. Returns false when `s` is on or too close to an existing boundary.
    pub fn split_u(&mut self, s: f64) -> bool {
        let (pi, a) = Self::locate(&self.us, s);
        if !(0.01..=0.99).contains(&a) {
            return false;
        }
        let (nx, ny) = (self.nx(), self.ny());
        let mut pts = Vec::with_capacity((nx + 3) * ny);
        for j in 0..ny {
            let row = &self.points[j * nx..(j + 1) * nx];
            pts.extend_from_slice(&row[..3 * pi]);
            let seg = [row[3 * pi], row[3 * pi + 1], row[3 * pi + 2], row[3 * pi + 3]];
            pts.extend_from_slice(&split_cubic(seg, a));
            pts.extend_from_slice(&row[3 * pi + 4..]);
        }
        self.points = pts;
        self.us.insert(pi + 1, s.clamp(0.0, 1.0));
        true
    }

    /// Splits the patch row containing `t` at `t` (a horizontal split line). Exact.
    pub fn split_v(&mut self, t: f64) -> bool {
        self.transpose();
        let ok = self.split_u(t);
        self.transpose();
        ok
    }

    /// Removes interior vertical boundary `k` (1..us.len()-1), merging its two patch columns.
    /// Exact when the patches came from an unedited split; otherwise the outer control points are
    /// kept and the inner handles extrapolated.
    pub fn remove_split_u(&mut self, k: usize) -> bool {
        if k == 0 || k + 1 >= self.us.len() {
            return false;
        }
        let a = (self.us[k] - self.us[k - 1]) / (self.us[k + 1] - self.us[k - 1]);
        let (nx, ny) = (self.nx(), self.ny());
        let mut pts = Vec::with_capacity((nx - 3) * ny);
        let c0 = 3 * (k - 1);
        for j in 0..ny {
            let row = &self.points[j * nx..(j + 1) * nx];
            let (p0, q1, r2, p3) = (row[c0], row[c0 + 1], row[c0 + 5], row[c0 + 6]);
            // De Casteljau inverse: q1 = lerp(p0, p1, a), r2 = lerp(p2, p3, a).
            let p1 = [(q1[0] - (1.0 - a) * p0[0]) / a, (q1[1] - (1.0 - a) * p0[1]) / a];
            let p2 = [(r2[0] - a * p3[0]) / (1.0 - a), (r2[1] - a * p3[1]) / (1.0 - a)];
            pts.extend_from_slice(&row[..c0]);
            pts.extend_from_slice(&[p0, p1, p2, p3]);
            pts.extend_from_slice(&row[c0 + 7..]);
        }
        self.points = pts;
        self.us.remove(k);
        true
    }

    /// Removes interior horizontal boundary `k`.
    pub fn remove_split_v(&mut self, k: usize) -> bool {
        self.transpose();
        let ok = self.remove_split_u(k);
        self.transpose();
        ok
    }

    fn transpose(&mut self) {
        let (nx, ny) = (self.nx(), self.ny());
        let mut pts = vec![[0.0; 2]; nx * ny];
        for j in 0..ny {
            for i in 0..nx {
                pts[i * ny + j] = self.points[j * nx + i];
            }
        }
        self.points = pts;
        std::mem::swap(&mut self.us, &mut self.vs);
    }

    /// Applies `f` to every control point (exact for affine maps).
    pub fn map_points(&self, f: impl Fn([f64; 2]) -> [f64; 2]) -> BezierMesh {
        BezierMesh { us: self.us.clone(), vs: self.vs.clone(), points: self.points.iter().map(|p| f(*p)).collect() }
    }

    /// Normalised `(s, t)` whose surface point is nearest `p` (coarse search, then Newton
    /// steps on the patch). Used to place split lines where the user clicks.
    pub fn param_at(&self, p: [f64; 2]) -> (f64, f64) {
        let n = 48;
        let mut best = (0.5, 0.5, f64::MAX);
        for j in 0..=n {
            for i in 0..=n {
                let (s, t) = (i as f64 / n as f64, j as f64 / n as f64);
                let q = self.eval(s, t);
                let d = (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2);
                if d < best.2 {
                    best = (s, t, d);
                }
            }
        }
        let (mut s, mut t) = (best.0, best.1);
        for _ in 0..12 {
            let h = 1e-5;
            let q = self.eval(s, t);
            let (qs, qt) = (self.eval(s + h, t), self.eval(s, t + h));
            let (a, b, c, d) = ((qs[0] - q[0]) / h, (qt[0] - q[0]) / h, (qs[1] - q[1]) / h, (qt[1] - q[1]) / h);
            let det = a * d - b * c;
            if det.abs() < 1e-12 {
                break;
            }
            let (ex, ey) = (p[0] - q[0], p[1] - q[1]);
            s = (s + (d * ex - b * ey) / det).clamp(0.0, 1.0);
            t = (t + (a * ey - c * ex) / det).clamp(0.0, 1.0);
        }
        (s, t)
    }

    /// Bounding box of the control points (the surface lies inside it).
    pub fn control_bounds(&self) -> [f64; 4] {
        self.points.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])])
    }
}

/// A complete warp: what Photoshop's `warp` descriptor stores, plus the custom mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Warp {
    pub style: WarpStyle,
    /// Bend in percent (-100..100), presets only.
    #[serde(default)]
    pub bend: f64,
    #[serde(default)]
    pub h_distort: f64,
    #[serde(default)]
    pub v_distort: f64,
    /// Warp along the vertical axis.
    #[serde(default)]
    pub vertical: bool,
    /// The source box `[x0, y0, x1, y1]` the warp is defined over.
    pub bounds: [f64; 4],
    /// Control mesh for [`WarpStyle::Custom`] (output coordinates).
    #[serde(default)]
    pub mesh: Option<BezierMesh>,
}

impl Warp {
    /// No warp over `bounds`.
    pub fn none(bounds: [f64; 4]) -> Warp {
        Warp { style: WarpStyle::None, bend: 0.0, h_distort: 0.0, v_distort: 0.0, vertical: false, bounds, mesh: None }
    }

    /// A preset style.
    pub fn preset(style: WarpStyle, bend: f64, bounds: [f64; 4]) -> Warp {
        Warp { style, bend, ..Warp::none(bounds) }
    }

    /// A custom warp from a mesh.
    pub fn custom(mesh: BezierMesh, bounds: [f64; 4]) -> Warp {
        Warp { style: WarpStyle::Custom, mesh: Some(mesh), ..Warp::none(bounds) }
    }

    fn style_warp(&self) -> Option<StyleWarp> {
        StyleWarp::new(self.style, self.bend, self.h_distort, self.v_distort, self.vertical, self.bounds)
    }

    /// Whether the warp maps every point of its box onto itself (within `1e-9` px).
    pub fn is_identity(&self) -> bool {
        match self.style {
            WarpStyle::None => true,
            WarpStyle::Custom => match &self.mesh {
                None => true,
                Some(m) => {
                    let id = BezierMesh::fit(&|s, t| self.box_point(s, t), m.us.clone(), m.vs.clone());
                    m.points.iter().zip(&id.points).all(|(a, b)| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9)
                }
            },
            _ => self.style_warp().is_none(),
        }
    }

    fn box_point(&self, s: f64, t: f64) -> [f64; 2] {
        let b = self.bounds;
        [b[0] + s * (b[2] - b[0]), b[1] + t * (b[3] - b[1])]
    }

    /// Maps a source point (inside the box) to output space.
    pub fn map(&self, x: f64, y: f64) -> (f64, f64) {
        match self.style {
            WarpStyle::None => (x, y),
            WarpStyle::Custom => match &self.mesh {
                Some(m) => {
                    let b = self.bounds;
                    let (w, h) = (b[2] - b[0], b[3] - b[1]);
                    if w <= 0.0 || h <= 0.0 {
                        return (x, y);
                    }
                    let p = m.eval((x - b[0]) / w, (y - b[1]) / h);
                    (p[0], p[1])
                }
                None => (x, y),
            },
            _ => match self.style_warp() {
                Some(sw) => sw.apply(x, y),
                None => (x, y),
            },
        }
    }

    /// The warp as a control mesh (a preset is fitted with `cols × rows` patches; custom warps
    /// return their own mesh).
    pub fn to_mesh(&self, cols: usize, rows: usize) -> BezierMesh {
        if let (WarpStyle::Custom, Some(m)) = (self.style, &self.mesh) {
            return m.clone();
        }
        let (cols, rows) = (cols.clamp(1, 64), rows.clamp(1, 64));
        let us: Vec<f64> = (0..=cols).map(|i| i as f64 / cols as f64).collect();
        let vs: Vec<f64> = (0..=rows).map(|i| i as f64 / rows as f64).collect();
        BezierMesh::fit(
            &|s, t| {
                let p = self.box_point(s, t);
                let (x, y) = self.map(p[0], p[1]);
                [x, y]
            },
            us,
            vs,
        )
    }

    /// The same warp followed by the affine map `[a, b, c, d, e, f]` (`x' = a·x + c·y + e`,
    /// `y' = b·x + d·y + f`), as a custom mesh (exact for custom warps).
    pub fn then_affine(&self, m: [f64; 6]) -> Warp {
        let mesh = self.to_mesh(4, 4).map_points(|p| [m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5]]);
        Warp::custom(mesh, self.bounds)
    }

    /// Approximate output bounds (sampled; includes the control mesh for custom warps).
    pub fn output_bounds(&self) -> [f64; 4] {
        let n = 32;
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for j in 0..=n {
            for i in 0..=n {
                let p = self.box_point(i as f64 / n as f64, j as f64 / n as f64);
                let (x, y) = self.map(p[0], p[1]);
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: [f64; 4] = [10.0, 20.0, 110.0, 70.0];

    fn close(a: (f64, f64), b: (f64, f64), eps: f64) -> bool {
        (a.0 - b.0).abs() < eps && (a.1 - b.1).abs() < eps
    }

    #[test]
    fn styles_parse_and_round_trip_names() {
        for s in WarpStyle::all() {
            assert_eq!(WarpStyle::parse(s.id()), Some(s));
            assert_eq!(WarpStyle::parse(s.psd_name()), Some(s));
            assert_eq!(WarpStyle::parse(s.label()), Some(s), "{s:?}");
        }
        assert_eq!(WarpStyle::parse("spiral"), None);
    }

    #[test]
    fn presets_at_bend_zero_are_identity() {
        for s in WarpStyle::PRESETS {
            let w = Warp::preset(s, 0.0, B);
            assert!(w.is_identity());
            assert_eq!(w.map(33.0, 44.0), (33.0, 44.0));
        }
    }

    #[test]
    fn identity_mesh_is_exact_and_fit_reproduces_affine() {
        let m = BezierMesh::identity(B, 1, 1);
        assert_eq!((m.nx(), m.ny()), (4, 4));
        let w = Warp::custom(m, B);
        assert!(w.is_identity());
        assert!(close(w.map(37.5, 61.25), (37.5, 61.25), 1e-9));
        // Affine maps fit exactly.
        let f = |s: f64, t: f64| [3.0 + 2.0 * s - t, 1.0 + 0.5 * s + 4.0 * t];
        let m = BezierMesh::fit(&f, vec![0.0, 1.0], vec![0.0, 0.4, 1.0]);
        for (s, t) in [(0.1, 0.9), (0.5, 0.2), (0.77, 0.41)] {
            let p = m.eval(s, t);
            let e = f(s, t);
            assert!((p[0] - e[0]).abs() < 1e-9 && (p[1] - e[1]).abs() < 1e-9);
        }
    }

    #[test]
    fn splits_are_exact_and_removable() {
        let mut m = BezierMesh::identity(B, 1, 1);
        // Bend it: move an inner control point.
        m.points[5] = [50.0, 10.0];
        let before: Vec<[f64; 2]> = (0..=10).flat_map(|j| (0..=10).map(move |i| (i, j))).map(|(i, j)| m.eval(i as f64 / 10.0, j as f64 / 10.0)).collect();
        let orig = m.clone();
        assert!(m.split_u(0.3));
        assert!(m.split_v(0.6));
        assert!(!m.split_u(0.3), "existing boundary");
        assert_eq!((m.nx(), m.ny()), (7, 7));
        assert!(m.is_valid());
        let after: Vec<[f64; 2]> = (0..=10).flat_map(|j| (0..=10).map(move |i| (i, j))).map(|(i, j)| m.eval(i as f64 / 10.0, j as f64 / 10.0)).collect();
        for (a, b) in before.iter().zip(&after) {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
        }
        let (ls, lt) = m.param_at(m.eval(0.42, 0.77));
        assert!((ls - 0.42).abs() < 1e-6 && (lt - 0.77).abs() < 1e-6, "{ls},{lt}");
        assert!(m.remove_split_u(1));
        assert!(m.remove_split_v(1));
        assert_eq!(m.us, orig.us);
        for (a, b) in m.points.iter().zip(&orig.points) {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
        }
    }

    #[test]
    fn arc_maps_known_points() {
        // Arc, bend 50 %: the centre column stays, the ends drop and pull in along a circle.
        let w = Warp::preset(WarpStyle::Arc, 50.0, [0.0, -40.0, 200.0, 10.0]);
        let mid = w.map(100.0, -15.0);
        assert!(close(mid, (100.0, -15.0), 1e-9));
        // Centre line: radius r = half / θmax with half = 100/25 = 4 (in half-heights), θmax = π/4.
        let (theta, r) = (std::f64::consts::FRAC_PI_4, 4.0 / std::f64::consts::FRAC_PI_4);
        let ex = 100.0 - r * theta.sin() * 100.0 / 4.0;
        let ey = -15.0 + (r - r * theta.cos()) * 25.0;
        let end = w.map(0.0, -15.0);
        assert!(close(end, (ex, ey), 1e-6), "{end:?} vs {:?}", (ex, ey));
    }

    #[test]
    fn preset_fits_closely_as_mesh_and_affine_composes() {
        let w = Warp::preset(WarpStyle::Bulge, 40.0, B);
        let m = Warp::custom(w.to_mesh(4, 4), B);
        for (x, y) in [(20.0, 30.0), (60.0, 45.0), (100.0, 65.0)] {
            assert!(close(w.map(x, y), m.map(x, y), 0.5), "{:?} vs {:?}", w.map(x, y), m.map(x, y));
        }
        let t = Warp::custom(BezierMesh::identity(B, 1, 1), B).then_affine([1.0, 0.0, 0.0, 1.0, 5.0, -2.0]);
        assert!(close(t.map(50.0, 50.0), (55.0, 48.0), 1e-9));
        let ob = Warp::none(B).output_bounds();
        assert!((ob[0] - 10.0).abs() < 1e-9 && (ob[3] - 70.0).abs() < 1e-9);
    }
}
