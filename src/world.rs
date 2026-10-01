//! A 240×120 land mask (1.5° cells, equirectangular, row 0 at 90°N,
//! column 0 at 180°W), rasterised from Natural Earth 110m land
//! (world-atlas 2.0.2, public domain). One bit per cell, MSB first.

const W: usize = 240;
const H: usize = 120;
static LAND: &[u8; W * H / 8] = include_bytes!("../assets/land-240x120.bin");

/// Mask width in cells.
pub const WIDTH: usize = W;
/// Mask height in cells.
pub const HEIGHT: usize = H;

/// Whether cell (`x`, `y`) is land.
pub fn land(x: usize, y: usize) -> bool {
    if x >= W || y >= H {
        return false;
    }
    let i = y * W + x;
    LAND[i / 8] & (0x80 >> (i % 8)) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn land_at(lat: f64, lon: f64) -> bool {
        land(((lon + 180.0) / 1.5) as usize, ((90.0 - lat) / 1.5) as usize)
    }

    #[test]
    fn known_places() {
        assert!(land_at(48.85, 2.35), "Paris");
        assert!(land_at(-23.5, -46.6), "São Paulo");
        assert!(land_at(35.7, 139.7), "Tokyo");
        assert!(!land_at(0.0, -140.0), "Pacific");
        assert!(!land_at(30.0, -40.0), "Atlantic");
    }
}
