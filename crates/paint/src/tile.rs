//! Grayscale bitmaps for sampled tips and texture patterns, with a compact serde form.
//!
//! Values are 16-bit (0..65535 ↔ 0..1) so presets stay small; JSON carries them as base64 of the
//! little-endian samples (`"data": "…"`), and also accepts a plain array of 0..1 floats.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A grayscale bitmap, row-major, 1 = full.
#[derive(Clone, Debug, PartialEq)]
pub struct GrayTile {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u16>,
}

impl GrayTile {
    /// Build from a function returning 0..1 values.
    pub fn from_fn(width: u32, height: u32, f: impl Fn(u32, u32) -> f32) -> Self {
        let mut data = Vec::with_capacity(width as usize * height as usize);
        for y in 0..height {
            for x in 0..width {
                data.push(q16(f(x, y)));
            }
        }
        Self { width, height, data }
    }
    /// Build from 0..1 floats (`width × height`).
    pub fn from_f32(width: u32, height: u32, v: &[f32]) -> Self {
        assert_eq!(v.len(), width as usize * height as usize, "GrayTile size mismatch");
        Self { width, height, data: v.iter().map(|&x| q16(x)).collect() }
    }
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> f32 {
        f32::from(self.data[(y * self.width + x) as usize]) / 65535.0
    }
    pub fn to_f32(&self) -> Vec<f32> {
        self.data.iter().map(|&v| f32::from(v) / 65535.0).collect()
    }
    pub fn is_valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.data.len() == self.width as usize * self.height as usize
    }
}

#[inline]
fn q16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

// ---------- serde ----------

#[derive(Serialize)]
struct GrayTileOut<'a> {
    width: u32,
    height: u32,
    data: &'a str,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DataIn {
    B64(String),
    Floats(Vec<f32>),
}

#[derive(Deserialize)]
struct GrayTileIn {
    width: u32,
    height: u32,
    data: DataIn,
}

impl Serialize for GrayTile {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut bytes = Vec::with_capacity(self.data.len() * 2);
        for v in &self.data {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let enc = b64_encode(&bytes);
        GrayTileOut { width: self.width, height: self.height, data: &enc }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for GrayTile {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let t = GrayTileIn::deserialize(d)?;
        let n = t.width as usize * t.height as usize;
        let data: Vec<u16> = match t.data {
            DataIn::B64(s) => {
                let b = b64_decode(&s).ok_or_else(|| D::Error::custom("invalid base64 in tile data"))?;
                b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
            }
            DataIn::Floats(v) => v.iter().map(|&x| q16(x)).collect(),
        };
        if n == 0 || data.len() != n {
            return Err(D::Error::custom(format!("tile data has {} samples, expected {}×{}", data.len(), t.width, t.height)));
        }
        Ok(GrayTile { width: t.width, height: t.height, data })
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (the tile serde form; commands taking file bytes).
pub fn b64_encode(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Decode standard base64 (whitespace ignored); `None` on invalid input.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for c in s.chunks(4) {
        let pad = c.iter().rev().take_while(|&&x| x == b'=').count();
        let mut n = 0u32;
        for (i, &x) in c.iter().enumerate() {
            n = (n << 6) | if i >= 4 - pad { 0 } else { val(x)? };
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ---------- sampling ----------

/// A float mip pyramid of a [`GrayTile`] for sampling at any scale without aliasing.
#[derive(Clone, Debug)]
pub struct Mips {
    /// (width, height, values), level 0 = full resolution.
    pub levels: Vec<(usize, usize, Vec<f32>)>,
}

impl Mips {
    pub fn new(t: &GrayTile) -> Self {
        let mut levels = vec![(t.width as usize, t.height as usize, t.to_f32())];
        while let Some((w, h, v)) = levels.last() {
            if *w <= 1 && *h <= 1 {
                break;
            }
            let (nw, nh) = ((*w).div_ceil(2), (*h).div_ceil(2));
            let mut nv = vec![0.0f32; nw * nh];
            for y in 0..nh {
                for x in 0..nw {
                    let mut s = 0.0;
                    let mut c = 0.0;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let (sx, sy) = (x * 2 + dx, y * 2 + dy);
                            if sx < *w && sy < *h {
                                s += v[sy * w + sx];
                                c += 1.0;
                            }
                        }
                    }
                    nv[y * nw + x] = s / c;
                }
            }
            levels.push((nw, nh, nv));
        }
        Self { levels }
    }

    /// Mip level for drawing the tile at `px` pixels across its larger side.
    pub fn level_for(&self, px: f32) -> usize {
        let (w, h, _) = &self.levels[0];
        let ratio = (*w.max(h) as f32 / px.max(1.0)).max(1.0);
        (ratio.log2().floor() as usize).min(self.levels.len() - 1)
    }

    /// Bilinear sample at normalised `u, v` in 0..1 (outside = 0).
    #[inline]
    pub fn sample_clamped(&self, level: usize, u: f32, v: f32) -> f32 {
        let (w, h, d) = &self.levels[level];
        let fx = u * *w as f32 - 0.5;
        let fy = v * *h as f32 - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let get = |x: i64, y: i64| if x < 0 || y < 0 || x >= *w as i64 || y >= *h as i64 { 0.0 } else { d[y as usize * w + x as usize] };
        let a = get(x0, y0) + (get(x0 + 1, y0) - get(x0, y0)) * tx;
        let b = get(x0, y0 + 1) + (get(x0 + 1, y0 + 1) - get(x0, y0 + 1)) * tx;
        a + (b - a) * ty
    }
}

/// A tiling float pattern (texture), sampled with wrap-around.
#[derive(Clone, Debug)]
pub struct PatternImage {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl PatternImage {
    #[inline]
    pub fn sample_wrap(&self, x: f32, y: f32) -> f32 {
        let xi = (x.floor() as i64).rem_euclid(self.width as i64) as usize;
        let yi = (y.floor() as i64).rem_euclid(self.height as i64) as usize;
        self.data[yi * self.width + xi]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..10 {
            let b: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&b)).unwrap(), b);
        }
    }

    #[test]
    fn tile_serde_round_trip_and_float_form() {
        let t = GrayTile::from_fn(5, 3, |x, y| (x + y) as f32 / 6.0);
        let j = serde_json::to_string(&t).unwrap();
        let back: GrayTile = serde_json::from_str(&j).unwrap();
        assert_eq!(back, t);
        let f: GrayTile = serde_json::from_str(r#"{"width":2,"height":1,"data":[0.0,1.0]}"#).unwrap();
        assert_eq!(f.data, vec![0, 65535]);
        assert!(serde_json::from_str::<GrayTile>(r#"{"width":2,"height":2,"data":[0.0,1.0]}"#).is_err());
    }

    #[test]
    fn mips_average() {
        let t = GrayTile::from_fn(4, 4, |x, _| if x < 2 { 1.0 } else { 0.0 });
        let m = Mips::new(&t);
        assert_eq!(m.levels.len(), 3);
        assert!((m.levels[2].2[0] - 0.5).abs() < 1e-3);
        assert_eq!(m.level_for(4.0), 0);
        assert_eq!(m.level_for(1.0), 2);
    }
}
