//! Bézier flattening with a distance tolerance.

use photocraft_doc::{Path, Subpath};
use photocraft_geom::Point;

/// A flattened subpath.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polyline {
    pub pts: Vec<(f64, f64)>,
    /// `true` for points that are knots of the source path (joins apply there); `false` for
    /// points inserted while flattening a curve.
    pub knot: Vec<bool>,
    pub closed: bool,
}

impl Polyline {
    fn push(&mut self, p: (f64, f64), knot: bool) {
        if let Some(&last) = self.pts.last()
            && (last.0 - p.0).abs() < 1e-12
            && (last.1 - p.1).abs() < 1e-12
        {
            if knot && let Some(k) = self.knot.last_mut() {
                *k = true;
            }
            return;
        }
        self.pts.push(p);
        self.knot.push(knot);
    }
}

/// Straight segment: both control points lie on the chord (within 1e-9 px) between its ends.
fn is_line(seg: &[Point; 4]) -> bool {
    let [p0, c1, c2, p1] = seg;
    let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
    let len2 = dx * dx + dy * dy;
    let on_chord = |c: &Point| {
        let (vx, vy) = (c.x - p0.x, c.y - p0.y);
        if len2 < 1e-24 {
            return vx.abs() < 1e-9 && vy.abs() < 1e-9;
        }
        let t = (vx * dx + vy * dy) / len2;
        let cross = (vx * dy - vy * dx).abs() / len2.sqrt();
        cross < 1e-9 && (-1e-9..=1.0 + 1e-9).contains(&t)
    };
    on_chord(c1) && on_chord(c2)
}

/// Number of uniform pieces so that the chord deviation of the cubic is at most `tol`
/// (bound `max|B''| / 8n²` with `max|B''| <= 6·max(|p0−2c1+c2|, |c1−2c2+p1|)`).
fn pieces(seg: &[Point; 4], tol: f64) -> usize {
    let [p0, c1, c2, p1] = seg;
    let d1 = ((p0.x - 2.0 * c1.x + c2.x).powi(2) + (p0.y - 2.0 * c1.y + c2.y).powi(2)).sqrt();
    let d2 = ((c1.x - 2.0 * c2.x + p1.x).powi(2) + (c1.y - 2.0 * c2.y + p1.y).powi(2)).sqrt();
    let m = d1.max(d2);
    // Small curves get a proportionally finer tolerance so their area stays accurate.
    let size = (c1.x - p0.x).hypot(c1.y - p0.y) + (c2.x - c1.x).hypot(c2.y - c1.y) + (p1.x - c2.x).hypot(p1.y - c2.y);
    let tol = tol.min(size * 1e-3).max(1e-4);
    let n = (0.75 * m / tol).sqrt().ceil();
    if n.is_finite() { (n as usize).clamp(1, 1 << 16) } else { 1 }
}

fn eval(seg: &[Point; 4], t: f64) -> (f64, f64) {
    let [p0, c1, c2, p1] = seg;
    let mt = 1.0 - t;
    let a = mt * mt * mt;
    let b = 3.0 * mt * mt * t;
    let c = 3.0 * mt * t * t;
    let d = t * t * t;
    (a * p0.x + b * c1.x + c * c2.x + d * p1.x, a * p0.y + b * c1.y + c * c2.y + d * p1.y)
}

/// Flattens one subpath. Closed subpaths do not repeat the first point at the end.
pub fn flatten_subpath(s: &Subpath, tol: f64) -> Polyline {
    let mut out = Polyline { closed: s.closed, ..Default::default() };
    if let Some(k) = s.knots.first() {
        out.push((k.anchor.x, k.anchor.y), true);
    }
    for seg in &s.segments() {
        if is_line(seg) {
            out.push((seg[3].x, seg[3].y), true);
            continue;
        }
        let n = pieces(seg, tol);
        for j in 1..=n {
            out.push(eval(seg, j as f64 / n as f64), j == n);
        }
    }
    if s.closed && out.pts.len() > 1 {
        let (a, b) = (out.pts[0], out.pts[out.pts.len() - 1]);
        if (a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12 {
            out.pts.pop();
            out.knot.pop();
            out.knot[0] = true;
        }
    }
    out
}

/// Flattens every subpath (see [`flatten_subpath`]).
pub fn flatten_path(p: &Path, tol: f64) -> Vec<Polyline> {
    p.subpaths.iter().map(|s| flatten_subpath(s, tol)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    #[test]
    fn lines_are_not_subdivided() {
        let s = Subpath::polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        let p = flatten_subpath(&s, 0.1);
        assert_eq!(p.pts.len(), 3);
        assert!(p.knot.iter().all(|k| *k));
    }

    #[test]
    fn circle_flattening_respects_tolerance() {
        for tol in [1.0, 0.25, 0.01] {
            let path = shapes::ellipse(0.0, 0.0, 200.0, 200.0);
            let p = flatten_subpath(&path.subpaths[0], tol);
            // Kappa circles deviate from the true circle by ~0.027% of r; chords add ≤ tol.
            for w in p.pts.windows(2) {
                let m = ((w[0].0 + w[1].0) / 2.0 - 100.0, (w[0].1 + w[1].1) / 2.0 - 100.0);
                let r = (m.0 * m.0 + m.1 * m.1).sqrt();
                assert!(100.0 - r <= tol + 0.03, "tol {tol}: chord mid at r={r}");
            }
            assert!(p.pts.len() < 4 * (1 + (0.75 * 100.0 / tol.min(0.15)).sqrt() as usize + 1));
        }
    }
}
