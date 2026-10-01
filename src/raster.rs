//! Pixel rendering of the peer map for terminals with an image protocol:
//! the GHOST rotating dot globe and the ANGEL reticle map, drawn into RGBA
//! and encoded as PNG.

use std::io::Write;

use crate::state::Place;
use crate::world;

/// An RGB colour.
pub type Rgb = [u8; 3];

/// An RGBA image.
pub struct Canvas {
    /// Width in pixels.
    pub w: usize,
    /// Height in pixels.
    pub h: usize,
    /// Row-major RGBA.
    pub px: Vec<u8>,
}

impl Canvas {
    /// A canvas filled with `bg` (opaque).
    pub fn new(w: usize, h: usize, bg: Rgb) -> Canvas {
        let mut px = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            px.extend_from_slice(&[bg[0], bg[1], bg[2], 255]);
        }
        Canvas { w, h, px }
    }

    /// Blends `c` at (`x`, `y`) with opacity `a` (0..1).
    pub fn blend(&mut self, x: i64, y: i64, c: Rgb, a: f64) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 || a <= 0.0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        let a = a.min(1.0);
        for k in 0..3 {
            let v = f64::from(self.px[i + k]) * (1.0 - a) + f64::from(c[k]) * a;
            self.px[i + k] = v.round().clamp(0.0, 255.0) as u8;
        }
    }

    /// A soft disc of radius `r`.
    pub fn dot(&mut self, x: f64, y: f64, r: f64, c: Rgb, a: f64) {
        let (x0, x1) = ((x - r - 1.0).floor() as i64, (x + r + 1.0).ceil() as i64);
        let (y0, y1) = ((y - r - 1.0).floor() as i64, (y + r + 1.0).ceil() as i64);
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d = ((px as f64 + 0.5 - x).powi(2) + (py as f64 + 0.5 - y).powi(2)).sqrt();
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                self.blend(px, py, c, a * cov);
            }
        }
    }

    /// A glow: a radial falloff of radius `r`.
    pub fn glow(&mut self, x: f64, y: f64, r: f64, c: Rgb, a: f64) {
        let (x0, x1) = ((x - r).floor() as i64, (x + r).ceil() as i64);
        let (y0, y1) = ((y - r).floor() as i64, (y + r).ceil() as i64);
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d = ((px as f64 - x).powi(2) + (py as f64 - y).powi(2)).sqrt() / r;
                if d < 1.0 {
                    self.blend(px, py, c, a * (1.0 - d).powi(2));
                }
            }
        }
    }

    /// A line, sampled every half pixel.
    pub fn line(&mut self, (x0, y0): (f64, f64), (x1, y1): (f64, f64), c: Rgb, a: f64) {
        let n = ((x1 - x0).abs().max((y1 - y0).abs()) * 2.0).ceil().max(1.0) as usize;
        for i in 0..=n {
            let t = i as f64 / n as f64;
            self.blend((x0 + (x1 - x0) * t) as i64, (y0 + (y1 - y0) * t) as i64, c, a * 0.6);
        }
    }

    /// A square outline.
    pub fn square(&mut self, x: f64, y: f64, r: f64, c: Rgb, a: f64) {
        self.line((x - r, y - r), (x + r, y - r), c, a);
        self.line((x + r, y - r), (x + r, y + r), c, a);
        self.line((x + r, y + r), (x - r, y + r), c, a);
        self.line((x - r, y + r), (x - r, y - r), c, a);
    }

    /// PNG (8-bit RGBA, zlib level 1).
    pub fn png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(self.h * (self.w * 4 + 1));
        for row in self.px.chunks(self.w * 4) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        let _ = z.write_all(&raw);
        let idat = z.finish().unwrap_or_default();
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(self.w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(self.h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &idat);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// Colours for a map.
pub struct MapColors {
    /// Background.
    pub bg: Rgb,
    /// Land and peers.
    pub acc: Rgb,
    /// Outbound peers, highlights.
    pub warn: Rgb,
    /// This node.
    pub hot: Rgb,
}

/// A peer to draw: position and whether it is outbound.
pub struct Pin {
    /// Where.
    pub place: Place,
    /// Outbound connection.
    pub out: bool,
}

fn land_points() -> impl Iterator<Item = (f64, f64)> {
    (0..world::HEIGHT).flat_map(|y| {
        (0..world::WIDTH)
            .filter(move |&x| world::land(x, y))
            .map(move |x| (90.0 - (y as f64 + 0.5) * 1.5, -180.0 + (x as f64 + 0.5) * 1.5))
    })
}

/// GHOST: an orthographic dot globe turned to `lon0` degrees, with arcs
/// and travelling packets from `here` to each peer at time `t` seconds.
pub fn globe(w: usize, h: usize, c: &MapColors, pins: &[Pin], here: Option<&Place>, lon0: f64, t: f64) -> Canvas {
    let mut cv = Canvas::new(w, h, c.bg);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let r = (w.min(h) as f64) * 0.44;
    let tilt: f64 = 0.32;
    let lon0 = lon0.to_radians();
    let proj = |lat: f64, lon: f64| {
        let (la, lo) = (lat.to_radians(), lon.to_radians() - lon0);
        let (x, y0, z0) = (la.cos() * lo.sin(), la.sin(), la.cos() * lo.cos());
        let y = y0 * tilt.cos() - z0 * tilt.sin();
        let z = y0 * tilt.sin() + z0 * tilt.cos();
        (cx + x * r, cy - y * r, z)
    };
    cv.glow(cx, cy, r * 1.25, [10, 60, 70], 0.55);
    for i in 0..720 {
        let a = i as f64 / 720.0 * std::f64::consts::TAU;
        cv.blend((cx + a.cos() * r) as i64, (cy + a.sin() * r) as i64, c.acc, 0.35);
    }
    let dot = (r / 160.0).max(0.7);
    for (la, lo) in land_points() {
        let (x, y, z) = proj(la, lo);
        if z > 0.0 {
            cv.dot(x, y, dot, c.acc, 0.12 + 0.6 * z);
        }
    }
    let origin = here.map(|p| proj(p.lat, p.lon)).filter(|o| o.2 > 0.0);
    for (i, p) in pins.iter().enumerate() {
        let (x, y, z) = proj(p.place.lat, p.place.lon);
        if z <= 0.0 {
            continue;
        }
        if let Some((ox, oy, _)) = origin {
            let (mx, my) = ((x + ox) / 2.0, (y + oy) / 2.0 - ((x - ox).hypot(y - oy)) * 0.25);
            let mut last = (ox, oy);
            for k in 1..=24 {
                let u = k as f64 / 24.0;
                let iu = 1.0 - u;
                let pt = (iu * iu * ox + 2.0 * iu * u * mx + u * u * x, iu * iu * oy + 2.0 * iu * u * my + u * u * y);
                cv.line(last, pt, c.acc, 0.25 * z);
                last = pt;
            }
            let u = (t * 0.35 + i as f64 * 0.137).fract();
            let iu = 1.0 - u;
            let pk = (iu * iu * ox + 2.0 * iu * u * mx + u * u * x, iu * iu * oy + 2.0 * iu * u * my + u * u * y);
            cv.dot(pk.0, pk.1, dot * 1.6, [255, 255, 255], 0.8 * z);
        }
        let col = if p.out { c.warn } else { c.acc };
        let pulse = 1.0 + 0.35 * (t * 3.0 + i as f64).sin();
        cv.glow(x, y, dot * 14.0 * z, col, 0.7);
        cv.dot(x, y, dot * 3.2 * pulse * z, col, 1.0);
    }
    if let Some((ox, oy, _)) = origin {
        cv.glow(ox, oy, dot * 12.0, c.hot, 0.7);
        cv.dot(ox, oy, dot * 3.0, c.hot, 1.0);
    }
    cv
}

/// ANGEL: an equirectangular dot map with a sweep at time `t` and square
/// reticles on peers (larger where the sweep passes).
pub fn flat(w: usize, h: usize, c: &MapColors, pins: &[Pin], here: Option<&Place>, t: f64) -> Canvas {
    let mut cv = Canvas::new(w, h, c.bg);
    let mw = (w as f64).min(h as f64 * 2.0);
    let (mh, ox) = (mw / 2.0, (w as f64 - mw) / 2.0);
    let oy = (h as f64 - mh) / 2.0;
    let p = |lat: f64, lon: f64| (ox + (lon + 180.0) / 360.0 * mw, oy + (90.0 - lat) / 180.0 * mh);
    for lon in (-150..=180).step_by(30) {
        let (x, _) = p(0.0, f64::from(lon));
        cv.line((x, oy), (x, oy + mh), c.acc, 0.18);
    }
    for lat in (-60..=60).step_by(30) {
        let (_, y) = p(f64::from(lat), 0.0);
        cv.line((ox, y), (ox + mw, y), c.acc, 0.18);
    }
    let cell = mw / world::WIDTH as f64;
    for (la, lo) in land_points() {
        let (x, y) = p(la, lo);
        cv.dot(x, y, (cell * 0.26).max(0.5), c.acc, 0.6);
    }
    let sx = ox + (t * 120.0) % mw;
    for k in 0..90 {
        let x = sx - k as f64;
        let a = 0.22 * (1.0 - k as f64 / 90.0);
        for y in (oy as i64)..((oy + mh) as i64) {
            cv.blend(x as i64, y, c.acc, a);
        }
    }
    for pin in pins {
        let (x, y) = p(pin.place.lat, pin.place.lon);
        let near = (x - sx).abs() < 70.0;
        let col = if near { c.warn } else { c.acc };
        let r = if near { cell * 5.0 } else { cell * 3.0 }.max(3.0);
        cv.square(x, y, r, col, 1.0);
        cv.line((x - r - 4.0, y), (x - r, y), col, 1.0);
        cv.line((x + r, y), (x + r + 4.0, y), col, 1.0);
        cv.dot(x, y, 1.2, col, 1.0);
    }
    if let Some(h) = here {
        let (x, y) = p(h.lat, h.lon);
        let pulse = 1.0 + 0.4 * (t * 5.0).sin();
        cv.glow(x, y, cell * 10.0, c.hot, 0.6);
        cv.dot(x, y, cell * 2.2 * pulse, c.hot, 1.0);
    }
    cv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_well_formed() {
        let c = MapColors { bg: [0, 0, 0], acc: [94, 246, 224], warn: [255, 195, 90], hot: [255, 61, 127] };
        let pins = [Pin { place: Place { lat: 51.5, lon: -0.1, city: "London".into(), country: "GB".into() }, out: false }];
        for cv in [globe(320, 200, &c, &pins, None, 0.0, 1.0), flat(320, 200, &c, &pins, None, 1.0)] {
            let png = cv.png();
            assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
            assert!(png.len() > 100);
            // Something other than the background was drawn.
            assert!(cv.px.chunks(4).any(|p| p[..3] != [0, 0, 0]));
        }
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }
}

#[cfg(test)]
mod preview {
    use super::*;

    /// `RSQ_DASH_PREVIEW=dir cargo test -p rsq-dash preview -- --ignored`
    #[test]
    #[ignore]
    fn write_previews() {
        let Ok(dir) = std::env::var("RSQ_DASH_PREVIEW") else { return };
        let cities = [(40.7, -74.0), (51.5, -0.1), (52.5, 13.4), (35.7, 139.7), (1.35, 103.8), (-33.9, 151.2), (-23.5, -46.6), (37.8, -122.4), (50.1, 8.7), (25.2, 55.3), (19.1, 72.9), (43.7, -79.4)];
        let pins: Vec<Pin> = cities
            .iter()
            .enumerate()
            .map(|(i, &(lat, lon))| Pin { place: Place { lat, lon, city: String::new(), country: String::new() }, out: i % 4 == 0 })
            .collect();
        let here = Place { lat: 39.1, lon: -94.6, city: String::new(), country: String::new() };
        let ghost = MapColors { bg: [2, 7, 11], acc: [94, 246, 224], warn: [255, 195, 90], hot: [255, 61, 127] };
        let angel = MapColors { bg: [6, 2, 3], acc: [255, 122, 26], warn: [255, 178, 26], hot: [255, 38, 38] };
        let _ = std::fs::write(format!("{dir}/globe.png"), globe(720, 440, &ghost, &pins, Some(&here), -60.0, 2.0).png());
        let _ = std::fs::write(format!("{dir}/flat.png"), flat(900, 440, &angel, &pins, Some(&here), 4.0).png());
    }
}
