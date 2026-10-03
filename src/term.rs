//! Modern-terminal extras: the kitty graphics protocol (kitty, Ghostty,
//! WezTerm) for a pixel peer map, the window title, and desktop
//! notifications (OSC 99 on kitty, OSC 9 elsewhere).

use std::io::Write;

/// The terminal we are in, as far as the environment tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// kitty.
    Kitty,
    /// Ghostty.
    Ghostty,
    /// WezTerm.
    WezTerm,
    /// Anything else.
    Other,
}

/// Detects the terminal from the environment.
pub fn detect() -> Kind {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let prog = var("TERM_PROGRAM").to_ascii_lowercase();
    if var("TERM").contains("kitty") || !var("KITTY_WINDOW_ID").is_empty() {
        Kind::Kitty
    } else if prog == "ghostty"
        || !var("GHOSTTY_RESOURCES_DIR").is_empty()
        || var("TERM").contains("ghostty")
    {
        Kind::Ghostty
    } else if prog == "wezterm" {
        Kind::WezTerm
    } else {
        Kind::Other
    }
}

impl Kind {
    /// Supports the kitty graphics protocol.
    pub fn graphics(self) -> bool {
        self != Kind::Other
    }
}

/// Pixel size of one cell, from the terminal (`(9, 18)` if it won't say).
pub fn cell_px() -> (u32, u32) {
    match crossterm::terminal::window_size() {
        Ok(ws) if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 => (
            u32::from(ws.width) / u32::from(ws.columns),
            u32::from(ws.height) / u32::from(ws.rows),
        ),
        _ => (9, 18),
    }
}

/// Image id used for the map.
pub const MAP_IMAGE: u32 = 7_701;

/// Places a PNG over `cols`×`rows` cells at (`col`, `row`), replacing any
/// earlier image with the same id. The cursor is saved and restored.
pub fn place_png(
    out: &mut impl Write,
    id: u32,
    png: &[u8],
    (col, row): (u16, u16),
    (cols, rows): (u16, u16),
) -> std::io::Result<()> {
    use base64_engine::encode;
    let data = encode(png);
    write!(out, "\x1b7\x1b[{};{}H", row + 1, col + 1)?;
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
    for (i, c) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let body = std::str::from_utf8(c).unwrap_or("");
        if i == 0 {
            write!(
                out,
                "\x1b_Ga=T,f=100,i={id},p=1,q=2,C=1,z=1,c={cols},r={rows},m={more};{body}\x1b\\"
            )?;
        } else {
            write!(out, "\x1b_Gm={more};{body}\x1b\\")?;
        }
    }
    write!(out, "\x1b8")?;
    out.flush()
}

/// Removes an image and its placements.
pub fn delete_image(out: &mut impl Write, id: u32) -> std::io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
    out.flush()
}

/// Sets the window title.
pub fn title(out: &mut impl Write, text: &str) -> std::io::Result<()> {
    write!(out, "\x1b]2;{}\x07", text.replace(['\x07', '\x1b'], ""))?;
    out.flush()
}

/// Sends a desktop notification.
pub fn notify(out: &mut impl Write, kind: Kind, title: &str, body: &str) -> std::io::Result<()> {
    let clean = |s: &str| s.replace(['\x07', '\x1b', ';'], " ");
    match kind {
        Kind::Kitty => write!(
            out,
            "\x1b]99;i=1:d=0;{}\x1b\\\x1b]99;i=1:d=1:p=body;{}\x1b\\",
            clean(title),
            clean(body)
        )?,
        _ => write!(out, "\x1b]9;{}: {}\x07", clean(title), clean(body))?,
    }
    out.flush()
}

/// Standard base64 (the protocol's payload encoding).
mod base64_engine {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
        for c in b.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            s.push(char::from(T[(n >> 18) as usize & 63]));
            s.push(char::from(T[(n >> 12) as usize & 63]));
            s.push(if c.len() > 1 {
                char::from(T[(n >> 6) as usize & 63])
            } else {
                '='
            });
            s.push(if c.len() > 2 {
                char::from(T[n as usize & 63])
            } else {
                '='
            });
        }
        s
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn rfc4648() {
            assert_eq!(super::encode(b""), "");
            assert_eq!(super::encode(b"f"), "Zg==");
            assert_eq!(super::encode(b"fo"), "Zm8=");
            assert_eq!(super::encode(b"foobar"), "Zm9vYmFy");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_escape_is_chunked() {
        let mut out = Vec::new();
        place_png(&mut out, 5, &vec![7u8; 10_000], (3, 4), (20, 10)).ok();
        let s = String::from_utf8(out).unwrap_or_default();
        assert!(s.starts_with("\x1b7\x1b[5;4H\x1b_Ga=T,f=100,i=5,"));
        assert!(s.contains("c=20,r=10,m=1;"));
        assert!(s.contains("\x1b_Gm=0;"));
        assert!(s.ends_with("\x1b8"));
    }
}
