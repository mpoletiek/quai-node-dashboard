//! The terminal dashboard: ratatui over crossterm, ~12 frames a second,
//! two looks (GHOST: a cyan cyberbrain HUD; ANGEL: an orange command-center
//! alarm board), toggled with `t`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CLine, Points};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Sparkline, Wrap};

use crate::Theme;
use crate::state::{Algo, State, now_ms};
use crate::world;

const FRAME: Duration = Duration::from_millis(83);
const BOOT_TICKS: u64 = 18;
const FLASH_TICKS: u64 = 16;
const HIST: usize = 48;

/// Colours of one theme.
#[derive(Clone, Copy)]
struct Pal {
    bg: Color,
    fg: Color,
    text: Color,
    dim: Color,
    faint: Color,
    alert: Color,
    warn: Color,
    ok: Color,
    purple: Color,
    scan: Color,
}

fn pal(theme: Theme) -> Pal {
    match theme {
        Theme::Ghost => Pal {
            bg: Color::Rgb(2, 7, 11),
            fg: Color::Rgb(94, 246, 224),
            text: Color::Rgb(186, 236, 230),
            dim: Color::Rgb(44, 128, 126),
            faint: Color::Rgb(16, 52, 58),
            alert: Color::Rgb(255, 61, 127),
            warn: Color::Rgb(255, 195, 90),
            ok: Color::Rgb(120, 255, 190),
            purple: Color::Rgb(150, 130, 255),
            scan: Color::Rgb(9, 34, 40),
        },
        Theme::Angel => Pal {
            bg: Color::Rgb(6, 2, 3),
            fg: Color::Rgb(255, 122, 26),
            text: Color::Rgb(255, 214, 170),
            dim: Color::Rgb(150, 66, 12),
            faint: Color::Rgb(56, 22, 8),
            alert: Color::Rgb(255, 38, 38),
            warn: Color::Rgb(255, 178, 26),
            ok: Color::Rgb(60, 255, 143),
            purple: Color::Rgb(150, 90, 255),
            scan: Color::Rgb(22, 8, 4),
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Dash,
    Logs,
    Map,
    Mining,
}

/// What a full-screen moment celebrates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Moment {
    Prime,
    Region,
    /// A block found through this node's stratum.
    Mined,
}

struct Flash {
    until: u64,
    kind: Moment,
    text: String,
}

/// Animation and view state kept between frames.
struct App {
    theme: Theme,
    tick: u64,
    view: View,
    help: bool,
    flash: Option<Flash>,
    last_event_ms: u64,
    last_zone: u64,
    zone_changed: u64,
    hist: [VecDeque<u64>; 3],
    hist_key: String,
    /// Pixel map on (kitty graphics protocol).
    gfx: bool,
    /// Where the last frame left room for the pixel map.
    map_rect: std::cell::Cell<Option<Rect>>,
}

/// Terminal options for the live TUI.
pub struct Options {
    /// Draw the peer map as an image (kitty graphics protocol).
    pub graphics: bool,
    /// Detected terminal.
    pub kind: crate::term::Kind,
    /// Desktop notifications for alerts.
    pub notify: bool,
}

impl App {
    fn new(theme: Theme) -> App {
        App {
            theme,
            tick: 0,
            view: View::Dash,
            help: false,
            flash: None,
            last_event_ms: now_ms(),
            last_zone: 0,
            zone_changed: 0,
            hist: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            hist_key: String::new(),
            gfx: false,
            map_rect: std::cell::Cell::new(None),
        }
    }

    /// Notices new blocks, events and mining samples.
    fn observe(&mut self, s: &State) {
        let zone = s.chains.zone.as_ref().map_or(0, |z| z.number);
        if zone != self.last_zone {
            if self.last_zone != 0 {
                self.zone_changed = self.tick;
            }
            self.last_zone = zone;
        }
        for e in s.events.iter().filter(|e| e.t_ms > self.last_event_ms) {
            let kind = match e.kind.as_str() {
                "prime" => Some(Moment::Prime),
                "region" => Some(Moment::Region),
                "mined" => Some(Moment::Mined),
                _ => None,
            };
            // A mined block outranks a region block arriving with it.
            let busy = self
                .flash
                .as_ref()
                .is_some_and(|f| f.kind == Moment::Mined && self.tick < f.until);
            if let Some(kind) = kind.filter(|k| *k == Moment::Mined || !busy) {
                self.flash = Some(Flash {
                    until: self.tick + FLASH_TICKS + if kind == Moment::Mined { 8 } else { 0 },
                    kind,
                    text: e.text.clone(),
                });
            }
        }
        if let Some(e) = s.events.back() {
            self.last_event_ms = self.last_event_ms.max(e.t_ms);
        }
        if let Some(m) = &s.mining {
            let key = format!(
                "{}/{}/{}",
                m.kawpow.hashrate, m.sha.hashrate, m.scrypt.hashrate
            );
            if key != self.hist_key {
                self.hist_key = key;
                for (h, a) in self.hist.iter_mut().zip([&m.kawpow, &m.sha, &m.scrypt]) {
                    h.push_back(scale_rate(a.hashrate));
                    while h.len() > HIST {
                        h.pop_front();
                    }
                }
            }
        }
    }

    /// Applies a key; true means quit.
    fn key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.help {
                    self.help = false;
                } else {
                    return true;
                }
            }
            KeyCode::Char('t') => {
                self.theme = if self.theme == Theme::Ghost {
                    Theme::Angel
                } else {
                    Theme::Ghost
                };
            }
            KeyCode::Char('l') => {
                self.view = if self.view == View::Logs {
                    View::Dash
                } else {
                    View::Logs
                }
            }
            KeyCode::Char('m') => {
                self.view = if self.view == View::Map {
                    View::Dash
                } else {
                    View::Map
                }
            }
            KeyCode::Char('s') => {
                self.view = if self.view == View::Mining {
                    View::Dash
                } else {
                    View::Mining
                }
            }
            KeyCode::Char('?') | KeyCode::Char('h') => self.help = !self.help,
            _ => {}
        }
        false
    }

    fn rand(&self, salt: u64) -> u64 {
        let mut x = self.tick.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ salt.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 29)
    }
}

/// Hashrate as a sparkline value (three significant digits of its decade).
fn scale_rate(h: f64) -> u64 {
    if h <= 0.0 {
        0
    } else {
        (h.log10() * 100.0).max(0.0) as u64
    }
}

/// Runs until the user quits.
pub fn run(state: Arc<Mutex<State>>, theme: Theme, opts: Options) -> Result<(), String> {
    let mut terminal = ratatui::try_init().map_err(|e| format!("terminal: {e}"))?;
    let result = event_loop(&mut terminal, &state, theme, &opts);
    if opts.graphics {
        let _ = crate::term::delete_image(&mut std::io::stdout(), crate::term::MAP_IMAGE);
    }
    ratatui::restore();
    result
}

/// Renders the pixel map for `rect` and places it, or removes it.
struct PixelMap {
    cell: (u32, u32),
    placed: bool,
    last: Option<(Rect, Theme)>,
}

impl PixelMap {
    fn update(&mut self, app: &App, s: &State) {
        use crate::raster::{MapColors, Pin, flat, globe};
        let mut out = std::io::stdout();
        let overlay = app.help
            || app.tick < BOOT_TICKS
            || app.flash.as_ref().is_some_and(|fl| app.tick < fl.until);
        let Some(rect) = app.map_rect.get().filter(|_| !overlay) else {
            if self.placed {
                let _ = crate::term::delete_image(&mut out, crate::term::MAP_IMAGE);
                self.placed = false;
                self.last = None;
            }
            return;
        };
        // About twice a second: every image crosses the terminal (and any
        // SSH link) whole.
        let fresh = self.last != Some((rect, app.theme));
        if !fresh && app.tick % 6 != 0 {
            return;
        }
        let w = (u32::from(rect.width) * self.cell.0).min(1600);
        let h = (u32::from(rect.height) * self.cell.1).min(1000);
        // A large map (full screen) is drawn at half size; the terminal
        // scales it to the same cells, at a quarter of the bytes.
        let half = u32::from(w * h > 360_000) + 1;
        let (w, h) = ((w / half) as usize, (h / half) as usize);
        if w < 16 || h < 16 {
            return;
        }
        let rgb = |c: Color| match c {
            Color::Rgb(r, g, b) => [r, g, b],
            _ => [128, 128, 128],
        };
        let p = pal(app.theme);
        let colors = MapColors {
            bg: rgb(p.bg),
            acc: rgb(p.fg),
            warn: rgb(p.warn),
            hot: rgb(p.alert),
        };
        let pins: Vec<Pin> = s
            .peers
            .list
            .iter()
            .filter_map(|q| {
                q.place.clone().map(|place| Pin {
                    place,
                    out: q.dir == "out",
                })
            })
            .collect();
        let t = app.tick as f64 * FRAME.as_secs_f64();
        let img = match app.theme {
            Theme::Ghost => globe(
                w,
                h,
                &colors,
                &pins,
                s.peers.here.as_ref(),
                -40.0 + t * 2.4,
                t,
            ),
            Theme::Angel => flat(w, h, &colors, &pins, s.peers.here.as_ref(), t),
        };
        if crate::term::place_png(
            &mut out,
            crate::term::MAP_IMAGE,
            &img.png(),
            (rect.x, rect.y),
            (rect.width, rect.height),
        )
        .is_ok()
        {
            self.placed = true;
            self.last = Some((rect, app.theme));
        }
    }
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: &Arc<Mutex<State>>,
    theme: Theme,
    opts: &Options,
) -> Result<(), String> {
    use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
    let mut app = App::new(theme);
    app.gfx = opts.graphics;
    let mut pixmap = PixelMap {
        cell: crate::term::cell_px(),
        placed: false,
        last: None,
    };
    let mut titled = 0u64;
    let mut alerted = now_ms();
    loop {
        let snap = state
            .lock()
            .map(|s| s.clone())
            .map_err(|_| "state lock poisoned".to_string())?;
        app.observe(&snap);
        let mut out = std::io::stdout();
        let _ = crossterm::execute!(out, BeginSynchronizedUpdate);
        terminal
            .draw(|f| draw(f, &app, &snap))
            .map_err(|e| e.to_string())?;
        if app.gfx {
            pixmap.update(&app, &snap);
        }
        let _ = crossterm::execute!(out, EndSynchronizedUpdate);
        // Window title follows the zone head.
        let zone = snap.chains.zone.as_ref().map_or(0, |z| z.number);
        if zone != titled {
            titled = zone;
            let _ = crate::term::title(
                &mut out,
                &format!("◆ {} · {} · quai-dash", thousands(zone), snap.node.location),
            );
        }
        // Desktop notifications for alerts.
        if opts.notify {
            for e in snap.events.iter().filter(|e| e.t_ms > alerted) {
                if matches!(e.kind.as_str(), "stall" | "offline" | "mismatch" | "reorg") {
                    let _ = crate::term::notify(
                        &mut out,
                        opts.kind,
                        &format!("quai-dash · {}", snap.node.label),
                        &e.text,
                    );
                }
            }
            if let Some(e) = snap.events.back() {
                alerted = alerted.max(e.t_ms);
            }
        }
        if event::poll(FRAME).map_err(|e| e.to_string())? {
            if let Event::Key(k) = event::read().map_err(|e| e.to_string())? {
                if k.kind == KeyEventKind::Press && app.key(k.code) {
                    return Ok(());
                }
            }
        }
        app.tick += 1;
    }
}

// ---------------------------------------------------------------- recording

fn hex_color(c: Color, default: &str) -> String {
    let named = |s: &str| s.to_string();
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset => named(default),
        Color::Black => named("#000000"),
        Color::Red => named("#cd3131"),
        Color::Green => named("#0dbc79"),
        Color::Yellow => named("#e5e510"),
        Color::Blue => named("#2472c8"),
        Color::Magenta => named("#bc3fbc"),
        Color::Cyan => named("#11a8cd"),
        Color::Gray => named("#a0a0a0"),
        Color::DarkGray => named("#666666"),
        Color::LightRed => named("#f14c4c"),
        Color::LightGreen => named("#23d18b"),
        Color::LightYellow => named("#f5f543"),
        Color::LightBlue => named("#3b8eea"),
        Color::LightMagenta => named("#d670d6"),
        Color::LightCyan => named("#29b8db"),
        Color::White => named("#e5e5e5"),
        Color::Indexed(i) => format!("#{0:02x}{0:02x}{0:02x}", i),
    }
}

/// East Asian wide characters occupy two cells.
fn wide(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6)
}

/// Renders `seconds` of the dashboard at `fps` into an off-screen buffer,
/// pressing `script` keys at the given seconds, and returns the frames as
/// JSON: a style table and, per frame, runs `[y, x, style, text]` of the
/// cells that changed (the first frame is complete).
pub fn record(
    state: &Arc<Mutex<State>>,
    theme: Theme,
    (w, h): (u16, u16),
    fps: u64,
    seconds: u64,
    script: &[(f64, char, &str)],
) -> Result<serde_json::Value, String> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut term = Terminal::new(TestBackend::new(w, h)).map_err(|e| e.to_string())?;
    let mut app = App::new(theme);
    let mut styles: Vec<String> = Vec::new();
    let mut style_ix = std::collections::HashMap::new();
    let mut prev: Option<ratatui::buffer::Buffer> = None;
    let mut frames = Vec::new();
    let mut captions = Vec::new();
    let mut next_key = 0usize;
    // Ticks advance at the live rate (83 ms) while frames are kept at `fps`.
    let total_ticks = seconds * 1000 / FRAME.as_millis() as u64;
    let every = (1000 / fps.max(1)).max(FRAME.as_millis() as u64) / FRAME.as_millis() as u64;
    for t in 0..total_ticks {
        let at = t as f64 * FRAME.as_secs_f64();
        while let Some(&(when, key, caption)) = script.get(next_key) {
            if when > at {
                break;
            }
            app.key(KeyCode::Char(key));
            captions.push(serde_json::json!([frames.len(), key.to_string(), caption]));
            next_key += 1;
        }
        let snap = state
            .lock()
            .map(|s| s.clone())
            .map_err(|_| "state lock poisoned".to_string())?;
        app.observe(&snap);
        term.draw(|f| draw(f, &app, &snap))
            .map_err(|e| e.to_string())?;
        if t % every.max(1) == 0 {
            let buf = term.backend().buffer().clone();
            let mut runs: Vec<serde_json::Value> = Vec::new();
            for y in 0..h {
                let mut x = 0u16;
                while x < w {
                    let changed = |xx: u16| {
                        prev.as_ref()
                            .is_none_or(|p| p.cell((xx, y)) != buf.cell((xx, y)))
                    };
                    if !changed(x) {
                        x += 1;
                        continue;
                    }
                    let Some(c0) = buf.cell((x, y)) else { break };
                    let st = c0.style();
                    let mut flags = 0u8;
                    let m = st.add_modifier;
                    if m.contains(Modifier::BOLD) {
                        flags |= 1;
                    }
                    if m.contains(Modifier::DIM) {
                        flags |= 2;
                    }
                    if m.contains(Modifier::ITALIC) {
                        flags |= 4;
                    }
                    if m.contains(Modifier::REVERSED) {
                        flags |= 8;
                    }
                    if m.contains(Modifier::UNDERLINED) {
                        flags |= 16;
                    }
                    let key = format!(
                        "{}|{}|{flags}",
                        hex_color(st.fg.unwrap_or(Color::Reset), "#d0d0d0"),
                        hex_color(st.bg.unwrap_or(Color::Reset), "#000000")
                    );
                    let ix = *style_ix.entry(key.clone()).or_insert_with(|| {
                        styles.push(key);
                        styles.len() - 1
                    });
                    let x0 = x;
                    let mut text = String::new();
                    while x < w {
                        let Some(c) = buf.cell((x, y)) else { break };
                        if c.style() != st || !changed(x) {
                            break;
                        }
                        let sym = c.symbol();
                        let sym = if sym.is_empty() { " " } else { sym };
                        text.push_str(sym);
                        x += if sym.chars().next().is_some_and(wide) {
                            2
                        } else {
                            1
                        };
                    }
                    if x == x0 {
                        x += 1;
                        continue;
                    }
                    runs.push(serde_json::json!([y, x0, ix, text]));
                }
            }
            frames.push(serde_json::Value::Array(runs));
            prev = Some(buf);
        }
        app.tick += 1;
        std::thread::sleep(FRAME);
    }
    let styles: Vec<serde_json::Value> = styles
        .iter()
        .map(|k| {
            let mut it = k.split('|');
            let fg = it.next().unwrap_or("#d0d0d0");
            let bg = it.next().unwrap_or("#000000");
            let fl: u8 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            serde_json::json!([fg, bg, fl])
        })
        .collect();
    let ms = FRAME.as_millis() as u64 * every.max(1);
    Ok(
        serde_json::json!({"w": w, "h": h, "frame_ms": ms, "styles": styles, "frames": frames, "captions": captions}),
    )
}

// ---------------------------------------------------------------- format

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn si_rate(h: f64) -> String {
    const UNITS: [&str; 7] = ["H/s", "KH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s"];
    let mut v = h.max(0.0);
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    format!("{v:.2} {}", UNITS[i])
}

fn age(secs: i64) -> String {
    if secs < 0 {
        "now".into()
    } else if secs < 120 {
        format!("{secs}s")
    } else if secs < 7200 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Decimal wei string → Gwei with separators.
fn gwei(dec: &str) -> String {
    let n: u128 = dec.parse().unwrap_or(0);
    format!("{} Gwei", thousands((n / 1_000_000_000) as u64))
}

/// Decimal 1e18-scaled string → `x.xxxx`.
fn e18(dec: &str) -> String {
    let n: u128 = dec.parse().unwrap_or(0);
    let int = n / 1_000_000_000_000_000_000;
    let frac = (n % 1_000_000_000_000_000_000) / 100_000_000_000_000;
    format!("{int}.{frac:04}")
}

fn short_hash(h: &str) -> String {
    crate::state::abbrev(h, 14, 8, 4)
}

fn utc_clock(ms: u64) -> String {
    let s = (ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}Z", s / 3600, (s / 60) % 60, s % 60)
}

fn group_digits(n: u64) -> String {
    thousands(n)
}

// ---------------------------------------------------------------- glyphs

const BIG: [[&str; 5]; 10] = [
    ["███", "█ █", "█ █", "█ █", "███"],
    [" █ ", "██ ", " █ ", " █ ", "███"],
    ["███", "  █", "███", "█  ", "███"],
    ["███", "  █", " ██", "  █", "███"],
    ["█ █", "█ █", "███", "  █", "  █"],
    ["███", "█  ", "███", "  █", "███"],
    ["███", "█  ", "███", "█ █", "███"],
    ["███", "  █", "  █", "  █", "  █"],
    ["███", "█ █", "███", "█ █", "███"],
    ["███", "█ █", "███", "  █", "███"],
];

/// Five rows of block digits for `s` (digits and commas).
fn big_rows(s: &str) -> [String; 5] {
    let mut rows: [String; 5] = Default::default();
    for c in s.chars() {
        for (r, row) in rows.iter_mut().enumerate() {
            match c.to_digit(10) {
                Some(d) => {
                    row.push_str(BIG[d as usize][r]);
                    row.push(' ');
                }
                None => row.push_str(if r == 4 { "▄ " } else { "  " }),
            }
        }
    }
    rows
}

/// Seven-segment digits (4 wide, 5 tall) for `s` (digits, `.` and `:`).
fn seg_rows(s: &str) -> [String; 5] {
    // a b c d e f g
    const SEG: [[bool; 7]; 10] = [
        [true, true, true, true, true, true, false],
        [false, true, true, false, false, false, false],
        [true, true, false, true, true, false, true],
        [true, true, true, true, false, false, true],
        [false, true, true, false, false, true, true],
        [true, false, true, true, false, true, true],
        [true, false, true, true, true, true, true],
        [true, true, true, false, false, false, false],
        [true, true, true, true, true, true, true],
        [true, true, true, true, false, true, true],
    ];
    let mut rows: [String; 5] = Default::default();
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            let g = SEG[d as usize];
            let h = |on: bool| if on { " ━━ " } else { "    " };
            let v = |l: bool, r: bool| {
                format!(
                    "{}  {}",
                    if l { "┃" } else { " " },
                    if r { "┃" } else { " " }
                )
            };
            rows[0].push_str(h(g[0]));
            rows[1].push_str(&v(g[5], g[1]));
            rows[2].push_str(h(g[6]));
            rows[3].push_str(&v(g[4], g[2]));
            rows[4].push_str(h(g[3]));
            for r in rows.iter_mut() {
                r.push(' ');
            }
        } else {
            let col = match c {
                '.' => [" ", " ", " ", " ", "▪"],
                ':' => [" ", "▪", " ", "▪", " "],
                _ => [" ", " ", " ", " ", " "],
            };
            for (r, x) in rows.iter_mut().zip(col) {
                r.push_str(x);
                r.push(' ');
            }
        }
    }
    rows
}

// ---------------------------------------------------------------- frames

/// A themed panel frame; returns the inner area.
fn panel(f: &mut Frame, area: Rect, app: &App, title: &str, jp: &str) -> Rect {
    let p = pal(app.theme);
    let block = match app.theme {
        Theme::Ghost => Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::new().fg(p.faint))
            .title(Line::from(vec![
                Span::styled(" ▸ ", Style::new().fg(p.fg)),
                Span::styled(
                    title.to_string(),
                    Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {jp} "), Style::new().fg(p.dim)),
            ])),
        Theme::Angel => Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::new().fg(p.dim))
            .title(Line::from(vec![
                Span::styled(
                    format!(" {title} "),
                    Style::new()
                        .fg(Color::Black)
                        .bg(p.fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {jp} "), Style::new().fg(p.warn)),
            ])),
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.theme == Theme::Ghost && area.width > 4 && area.height > 2 {
        // Bright corner ticks over the faint frame.
        let (l, r, t, b) = (
            area.x,
            area.x + area.width - 1,
            area.y,
            area.y + area.height - 1,
        );
        let buf = f.buffer_mut();
        for (x, y, sym) in [
            (l, t, "┌"),
            (r, t, "┐"),
            (l, b, "└"),
            (r, b, "┘"),
            (l + 1, b, "─"),
            (r - 1, b, "─"),
            (r - 1, t, "─"),
            (l, b - 1, "│"),
            (r, b - 1, "│"),
        ] {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_symbol(sym).set_fg(p.fg);
            }
        }
    }
    inner
}

fn kv<'a>(p: &Pal, k: &str, v: String, vc: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{k:<11}"), Style::new().fg(p.dim)),
        Span::styled(v, Style::new().fg(vc)),
    ])
}

// ---------------------------------------------------------------- draw

fn draw(f: &mut Frame, app: &App, s: &State) {
    app.map_rect.set(None);
    let p = pal(app.theme);
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::new().bg(p.bg).fg(p.text)),
        area,
    );
    if app.tick < BOOT_TICKS {
        draw_boot(f, app, s, area);
        return;
    }
    let header_h = if app.theme == Theme::Angel { 3 } else { 2 };
    let [head, body] =
        Layout::vertical([Constraint::Length(header_h), Constraint::Min(0)]).areas(area);
    draw_header(f, app, s, head);
    match app.view {
        View::Logs => {
            let inner = panel(f, body, app, "NODE LOG", "記録");
            draw_logs(f, app, s, inner);
        }
        View::Map => {
            let inner = panel(f, body, app, "PEER MAP", "地図");
            draw_map(f, app, s, inner);
        }
        View::Mining => draw_mining(f, app, s, body),
        View::Dash => draw_dash(f, app, s, body),
    }
    if let Some(fl) = app.flash.as_ref().filter(|fl| app.tick < fl.until) {
        draw_flash(f, app, fl, area);
    }
    if app.help {
        draw_help(f, app, area);
    }
    if app.theme == Theme::Ghost {
        scanline(f, app, area);
    }
}

fn draw_dash(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let compact = area.width < 100 || area.height < 28;
    if compact {
        let [top, algos, logs] = Layout::vertical([
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Min(4),
        ])
        .areas(area);
        let inner = panel(f, top, app, "ZONE HEIGHT", "鎖高");
        draw_hero(f, app, s, inner);
        draw_algos(f, app, s, algos);
        let inner = panel(f, logs, app, "NODE LOG", "記録");
        draw_logs(f, app, s, inner);
        return;
    }
    let tape_h = if area.height >= 41 { 7 } else { 0 };
    let [top, mid, tape, bottom] = Layout::vertical([
        Constraint::Length(11),
        Constraint::Length(12),
        Constraint::Length(tape_h),
        Constraint::Min(5),
    ])
    .areas(area);
    let [hero, chains, side] = Layout::horizontal([
        Constraint::Percentage(46),
        Constraint::Percentage(27),
        Constraint::Percentage(27),
    ])
    .areas(top);
    let inner = panel(f, hero, app, "ZONE HEIGHT", "鎖高");
    draw_hero(f, app, s, inner);
    let inner = panel(f, chains, app, "HIERARCHY", "階層");
    draw_chains(f, app, s, inner);
    let [peers, econ] = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(side);
    let inner = panel(f, peers, app, "PEERS", "接続");
    draw_peers(f, app, s, inner);
    let inner = panel(f, econ, app, "ECONOMY", "経済");
    draw_econ(f, app, s, inner);

    let [algos, map] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(mid);
    draw_algos(f, app, s, algos);
    let inner = panel(f, map, app, "PEER MAP", "地図");
    draw_map(f, app, s, inner);

    if tape_h > 0 {
        let inner = panel(f, tape, app, "BLOCK LATTICE", "階層");
        draw_tape(f, app, s, inner);
    }
    let (events, logs) = if s.stratum.is_some() {
        let [events, mining, logs] = Layout::horizontal([
            Constraint::Percentage(30),
            Constraint::Percentage(34),
            Constraint::Percentage(36),
        ])
        .areas(bottom);
        let inner = panel(f, mining, app, "STRATUM", "採掘");
        draw_stratum_brief(f, app, s, inner);
        (events, logs)
    } else {
        let [events, logs] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                .areas(bottom);
        (events, logs)
    };
    let inner = panel(f, events, app, "EVENTS", "事象");
    draw_events(f, app, s, inner);
    let inner = panel(f, logs, app, "NODE LOG", "記録");
    draw_logs(f, app, s, inner);
}

fn draw_header(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let now = now_ms();
    // The collector stamps the state every second; a state that stops
    // moving means it is stuck (a hung lookup, say), whatever it says.
    let stale = s.now_ms > 0 && now.saturating_sub(s.now_ms) > 10_000;
    let live = if stale {
        Span::styled(
            " ◌ STALE ",
            Style::new()
                .fg(Color::Black)
                .bg(p.warn)
                .add_modifier(Modifier::BOLD),
        )
    } else if s.node.online {
        Span::styled(
            " ● LIVE ",
            Style::new()
                .fg(Color::Black)
                .bg(p.ok)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " ✕ OFFLINE ",
            Style::new()
                .fg(Color::White)
                .bg(p.alert)
                .add_modifier(Modifier::BOLD),
        )
    };
    let location = if s.node.location.is_empty() {
        "—".to_string()
    } else {
        s.node.location.clone()
    };
    let chain = s.node.chain_id.map_or("—".to_string(), |c| c.to_string());
    let (brand, theme_name) = match app.theme {
        Theme::Ghost => ("QUAI//DIVE", "GHOST"),
        Theme::Angel => ("QUAI TERMINAL", "ANGEL"),
    };
    let info = Line::from(vec![
        Span::styled(
            format!(" {brand} "),
            Style::new().fg(p.bg).bg(p.fg).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            s.node.label.clone(),
            Style::new().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  ◇ {location}  ◇ CHAIN {chain}  "),
            Style::new().fg(p.dim),
        ),
        Span::styled(
            if s.node.kind.is_empty() {
                String::new()
            } else {
                format!("◇ {}  ", s.node.kind.to_uppercase())
            },
            Style::new().fg(p.text),
        ),
        live,
        Span::styled(format!("  {theme_name} ",), Style::new().fg(p.purple)),
        Span::styled(utc_clock(now), Style::new().fg(p.fg)),
        Span::styled(
            if s.stratum.is_some() {
                "   t theme · m map · s stratum · l log · ? help"
            } else {
                "   t theme · m map · l log · ? help"
            },
            Style::new().fg(p.faint),
        ),
    ]);
    match app.theme {
        Theme::Ghost => {
            let rule: String = (0..area.width)
                .map(|i| {
                    if (i as u64 + app.tick / 2) % 24 == 0 {
                        '╸'
                    } else {
                        '─'
                    }
                })
                .collect();
            let text = vec![info, Line::styled(rule, Style::new().fg(p.faint))];
            f.render_widget(Paragraph::new(text), area);
        }
        Theme::Angel => {
            let stripes = |off: u64| -> Line<'static> {
                Line::from(
                    (0..area.width)
                        .map(|i| {
                            let on = ((i as u64 + off) / 2) % 2 == 0;
                            Span::styled("▞", Style::new().fg(if on { p.fg } else { p.faint }))
                        })
                        .collect::<Vec<_>>(),
                )
            };
            let text = vec![stripes(app.tick / 2), info, stripes(app.tick / 2 + 1)];
            f.render_widget(Paragraph::new(text), area);
        }
    }
}

fn draw_hero(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    if !s.node.online {
        let err = s.node.error.clone().unwrap_or_else(|| "connecting…".into());
        let blink = (app.tick / 6) % 2 == 0;
        let title = match app.theme {
            Theme::Ghost => "LINK LOST // 接続断",
            Theme::Angel => "SIGNAL LOST — 信号途絶",
        };
        let text = vec![
            Line::raw(""),
            Line::styled(
                title,
                Style::new()
                    .fg(if blink { p.alert } else { p.warn })
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(format!("RPC {}", s.node.rpc), Style::new().fg(p.text)),
            Line::styled(err, Style::new().fg(p.alert)),
        ];
        f.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let zone = s.chains.zone.as_ref();
    let number = zone.map_or(0, |z| z.number);
    let since = zone.map_or(0.0, |z| {
        (now_ms() as f64 / 1000.0 - z.timestamp as f64).max(0.0)
    });
    let [left, right] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(24)]).areas(area);
    // Big height, scrambled for a moment after it changes (GHOST).
    let rows = big_rows(&group_digits(number));
    let fresh = app.tick.saturating_sub(app.zone_changed) < 4 && app.zone_changed > 0;
    let mut lines: Vec<Line> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let style = Style::new()
            .fg(if fresh && app.theme == Theme::Angel {
                p.warn
            } else {
                p.fg
            })
            .add_modifier(Modifier::BOLD);
        if fresh && app.theme == Theme::Ghost {
            const KANA: [char; 12] = [
                'ア', 'カ', 'サ', 'タ', 'ナ', 'ハ', 'マ', 'ヤ', 'ラ', 'ワ', 'ン', 'ヲ',
            ];
            let mut spans = Vec::new();
            for (i, c) in row.chars().enumerate() {
                if c == '█' && app.rand(r as u64 * 131 + i as u64) % 9 == 0 {
                    let k = KANA[(app.rand(i as u64) % KANA.len() as u64) as usize];
                    spans.push(Span::styled(k.to_string(), Style::new().fg(p.alert)));
                    // A kana is two cells wide: drop the next cell's glyph.
                } else {
                    spans.push(Span::styled(c.to_string(), style));
                }
            }
            lines.push(Line::from(spans));
        } else {
            lines.push(Line::styled(row.clone(), style));
        }
    }
    let avg = s.mining.as_ref().map_or(0.0, |m| m.avg_block_time);
    let last = s.blocks.back();
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("AVG ", Style::new().fg(p.dim)),
        Span::styled(format!("{avg:.2}s"), Style::new().fg(p.text)),
        Span::styled("  TXS ", Style::new().fg(p.dim)),
        Span::styled(
            last.map_or("—".into(), |b| b.txs.to_string()),
            Style::new().fg(p.text),
        ),
        Span::styled("  WS ", Style::new().fg(p.dim)),
        Span::styled(
            last.map_or("—".into(), |b| b.workshares.to_string()),
            Style::new().fg(p.text),
        ),
        Span::styled("  ", Style::new()),
        Span::styled(
            last.map_or(String::new(), |b| short_hash(&b.hash)),
            Style::new().fg(p.faint),
        ),
    ]));
    f.render_widget(Paragraph::new(lines), left);
    // Seven-segment timer: seconds since the last block.
    let late = avg > 0.0 && since > avg * 3.0;
    let timer = if since >= 100.0 {
        format!("{:03}", since as u64)
    } else {
        format!("{since:04.1}")
    };
    let label = match app.theme {
        Theme::Ghost => "SINCE LAST BLOCK",
        Theme::Angel => "活動限界 BLOCK TIMER",
    };
    let color = if late {
        p.alert
    } else if app.theme == Theme::Angel {
        p.warn
    } else {
        p.fg
    };
    let mut tl = vec![Line::styled(label, Style::new().fg(p.dim))];
    for row in seg_rows(&timer) {
        tl.push(Line::styled(
            row,
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    let gauge_w = right.width.saturating_sub(2) as f64;
    let frac = if avg > 0.0 {
        (since / (avg * 2.0)).min(1.0)
    } else {
        0.0
    };
    let filled = (gauge_w * frac) as usize;
    tl.push(Line::from(vec![
        Span::styled("▮".repeat(filled), Style::new().fg(color)),
        Span::styled(
            "▯".repeat((gauge_w as usize).saturating_sub(filled)),
            Style::new().fg(p.faint),
        ),
    ]));
    f.render_widget(Paragraph::new(tl), right);
}

fn draw_chains(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let now = now_ms() as i64 / 1000;
    let mut lines = Vec::new();
    for (name, head, c) in [
        ("PRIME", &s.chains.prime, p.alert),
        ("REGION", &s.chains.region, p.purple),
        ("ZONE", &s.chains.zone, p.fg),
    ] {
        let (num, ago) = match head {
            Some(h) => (thousands(h.number), age(secs_since(now, h.timestamp))),
            None => ("—".into(), String::new()),
        };
        let pulse = head
            .as_ref()
            .is_some_and(|h| secs_since(now, h.timestamp) < 2);
        lines.push(Line::from(vec![
            Span::styled(if pulse { "◆ " } else { "◇ " }, Style::new().fg(c)),
            Span::styled(
                format!("{name:<7}"),
                Style::new().fg(c).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{num:>11}"), Style::new().fg(p.text)),
            Span::styled(format!("  {ago}"), Style::new().fg(p.dim)),
        ]));
    }
    lines.push(Line::raw(""));
    match &s.compare {
        Some(c) => {
            let rate = if c.compared > 0 {
                c.matched as f64 * 100.0 / c.compared as f64
            } else {
                0.0
            };
            let ok = c.compared > 0 && c.matched == c.compared;
            let label = if app.theme == Theme::Angel {
                "シンクロ率"
            } else {
                "同期率"
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{label} SYNC "), Style::new().fg(p.dim)),
                Span::styled(
                    format!("{rate:5.1}%"),
                    Style::new()
                        .fg(if ok { p.ok } else { p.alert })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}/{}", c.matched, c.compared),
                    Style::new().fg(p.dim),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled(format!("⇄ {} ", c.label), Style::new().fg(p.text)),
                Span::styled(
                    if c.online {
                        thousands(c.height)
                    } else {
                        "offline".into()
                    },
                    Style::new().fg(if c.online { p.text } else { p.alert }),
                ),
            ]));
            if let Some(m) = c.last_mismatch {
                lines.push(Line::styled(
                    format!("DIFFERS at {}", thousands(m)),
                    Style::new().fg(p.alert),
                ));
            }
        }
        None => {
            if let Some(b) = s.blocks.back() {
                lines.push(kv(
                    &p,
                    "DIFFICULTY",
                    group_digits(b.difficulty.parse().unwrap_or(0)),
                    p.text,
                ));
                lines.push(kv(
                    &p,
                    "GAS",
                    format!("{} / {}", thousands(b.gas_used), thousands(b.gas_limit)),
                    p.text,
                ));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_peers(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let total = s.peers.inbound.saturating_add(s.peers.outbound).max(1) as f64;
    let w = area.width.saturating_sub(16) as f64;
    let bar = |n: u32, c: Color| -> Vec<Span<'static>> {
        let k = ((n as f64 / total) * w).round() as usize;
        vec![
            Span::styled("█".repeat(k), Style::new().fg(c)),
            Span::styled(
                "░".repeat((w as usize).saturating_sub(k)),
                Style::new().fg(p.faint),
            ),
        ]
    };
    let mut l1 = vec![Span::styled(
        format!("IN  {:>4} ", s.peers.inbound),
        Style::new().fg(p.text),
    )];
    l1.extend(bar(s.peers.inbound, p.fg));
    let mut l2 = vec![Span::styled(
        format!("OUT {:>4} ", s.peers.outbound),
        Style::new().fg(p.text),
    )];
    l2.extend(bar(s.peers.outbound, p.purple));
    let l3 = Line::from(vec![
        Span::styled(
            format!("TOTAL {}  ", s.peers.count),
            Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "MAPPED {}  GEO {}",
                s.peers.list.iter().filter(|x| x.place.is_some()).count(),
                s.peers.geo
            ),
            Style::new().fg(p.dim),
        ),
    ]);
    f.render_widget(
        Paragraph::new(vec![Line::from(l1), Line::from(l2), l3]),
        area,
    );
}

fn draw_econ(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let mut lines = Vec::new();
    if let Some(b) = s.blocks.back() {
        lines.push(kv(&p, "BASE FEE", gwei(&b.base_fee), p.text));
        lines.push(kv(&p, "kQUAI", e18(&b.exchange_rate), p.text));
    }
    if let Some(m) = &s.mining {
        lines.push(kv(
            &p,
            "REWARD",
            format!("{} QUAI", m.estimated_block_reward),
            p.fg,
        ));
        lines.push(kv(
            &p,
            "WORKSHARE",
            format!("{} QUAI", m.workshare_reward),
            p.text,
        ));
        lines.push(kv(
            &p,
            "SUPPLY",
            format!("{} QUAI", group_digits(m.quai_supply.parse().unwrap_or(0))),
            p.text,
        ));
    }
    if lines.is_empty() {
        lines.push(Line::styled("awaiting data…", Style::new().fg(p.dim)));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn algo_rows(s: &State) -> [(&'static str, &'static str, Algo, u32); 3] {
    let m = s.mining.clone().unwrap_or_default();
    [
        ("KAWPOW", "1", m.kawpow, s.pending_shares.kawpow),
        ("SHA", "2", m.sha, s.pending_shares.sha),
        ("SCRYPT", "3", m.scrypt, s.pending_shares.scrypt),
    ]
}

fn draw_algos(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    match app.theme {
        Theme::Ghost => {
            let inner = panel(f, area, app, "MERGED MINING", "演算");
            let rows = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(inner);
            for (i, (name, _, a, pend)) in algo_rows(s).into_iter().enumerate() {
                let Some(&row) = rows.get(i) else { continue };
                let [label, spark] =
                    Layout::horizontal([Constraint::Length(30), Constraint::Min(4)]).areas(row);
                let c = [p.fg, p.purple, p.warn][i];
                let text = vec![
                    Line::from(vec![
                        Span::styled(
                            format!("{name:<7}"),
                            Style::new().fg(c).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(si_rate(a.hashrate), Style::new().fg(p.text)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            format!("share {:>5.2}s", a.share_time),
                            Style::new().fg(p.dim),
                        ),
                        Span::styled(
                            format!("  pending {pend}"),
                            Style::new().fg(if pend > 0 { c } else { p.faint }),
                        ),
                    ]),
                ];
                f.render_widget(Paragraph::new(text), label);
                spark_right(f, &app.hist[i], c, spark);
            }
        }
        Theme::Angel => {
            // MAGI-style triad: three verdict panels.
            let cols = Layout::horizontal([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(area);
            for (i, (name, n, a, pend)) in algo_rows(s).into_iter().enumerate() {
                let Some(&col) = cols.get(i) else { continue };
                let approved = a.hashrate > 0.0 && a.share_time > 0.0;
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Thick)
                    .border_style(Style::new().fg(if approved { p.fg } else { p.alert }))
                    .title(Line::from(Span::styled(
                        format!(" {name}·{n} "),
                        Style::new()
                            .fg(Color::Black)
                            .bg(if approved { p.fg } else { p.alert })
                            .add_modifier(Modifier::BOLD),
                    )));
                let inner = block.inner(col);
                f.render_widget(block, col);
                let blink = approved || (app.tick / 5) % 2 == 0;
                let verdict = if approved {
                    "承認 APPROVED"
                } else {
                    "否決 DENIED"
                };
                let vstyle = if approved {
                    Style::new()
                        .fg(Color::Black)
                        .bg(p.ok)
                        .add_modifier(Modifier::BOLD)
                } else if blink {
                    Style::new()
                        .fg(Color::White)
                        .bg(p.alert)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(p.alert)
                };
                let text = vec![
                    Line::styled(
                        si_rate(a.hashrate),
                        Style::new().fg(p.warn).add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        format!("SHARE {:.2}s", a.share_time),
                        Style::new().fg(p.text),
                    ),
                    Line::styled(format!("PENDING {pend}"), Style::new().fg(p.text)),
                    Line::raw(""),
                    Line::styled(format!(" {verdict} "), vstyle),
                ];
                let [t, spark] =
                    Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(inner);
                f.render_widget(Paragraph::new(text).alignment(Alignment::Center), t);
                spark_right(f, &app.hist[i], p.dim, spark);
            }
        }
    }
}

/// A sparkline of `hist` drawn against the right edge of `area` (newest
/// sample rightmost), scaled to its own range.
fn spark_right(f: &mut Frame, hist: &VecDeque<u64>, color: Color, area: Rect) {
    if hist.is_empty() || area.width == 0 {
        return;
    }
    let n = hist.len().min(area.width as usize);
    let data: Vec<u64> = hist.iter().skip(hist.len() - n).copied().collect();
    let min = data.iter().copied().min().unwrap_or(0).saturating_sub(5);
    let shifted: Vec<u64> = data.iter().map(|v| v - min).collect();
    let r = Rect {
        x: area.x + area.width - n as u16,
        width: n as u16,
        ..area
    };
    f.render_widget(
        Sparkline::default()
            .data(&shifted)
            .style(Style::new().fg(color)),
        r,
    );
}

fn draw_map(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    if area.width < 10 || area.height < 4 {
        return;
    }
    if app.gfx {
        // Leave the area blank; the loop places the image over it.
        f.render_widget(Clear, area);
        f.render_widget(Block::default().style(Style::new().bg(p.bg)), area);
        app.map_rect.set(Some(Rect {
            height: area.height - 1,
            ..area
        }));
        let mapped = s.peers.list.iter().filter(|x| x.place.is_some()).count();
        let caption = if mapped == 0 {
            s.peers.note.clone().unwrap_or_else(|| {
                format!(
                    "{} peers; locations are off (--geoip off)",
                    s.peers.list.len()
                )
            })
        } else {
            format!(
                "{mapped} peers located · {} tcp · pixels",
                s.peers.list.len()
            )
        };
        let cap_area = Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        };
        f.render_widget(
            Paragraph::new(Line::styled(caption, Style::new().fg(p.dim)))
                .alignment(Alignment::Right),
            cap_area,
        );
        return;
    }
    let mut land = Vec::new();
    for y in 0..world::HEIGHT {
        for x in 0..world::WIDTH {
            if world::land(x, y) {
                land.push((
                    -180.0 + (x as f64 + 0.5) * 1.5,
                    90.0 - (y as f64 + 0.5) * 1.5,
                ));
            }
        }
    }
    let peers: Vec<(f64, f64)> = s
        .peers
        .list
        .iter()
        .filter_map(|x| x.place.as_ref())
        .map(|pl| (pl.lon, pl.lat))
        .collect();
    let here = s.peers.here.clone();
    let pulse = (app.tick / 4) % 2 == 0;
    let land_c = p.faint;
    let peer_c = if pulse { p.fg } else { p.text };
    let arc_c = p.dim;
    let alert = p.alert;
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([-180.0, 180.0])
        .y_bounds([-60.0, 85.0])
        .background_color(p.bg)
        .paint(move |ctx| {
            ctx.draw(&Points {
                coords: &land,
                color: land_c,
            });
            ctx.layer();
            if let Some(h) = &here {
                for &(x, y) in &peers {
                    ctx.draw(&CLine {
                        x1: h.lon,
                        y1: h.lat,
                        x2: x,
                        y2: y,
                        color: arc_c,
                    });
                }
            }
            ctx.layer();
            ctx.draw(&Points {
                coords: &peers,
                color: peer_c,
            });
            if let Some(h) = &here {
                ctx.print(h.lon, h.lat, Span::styled("◎", Style::new().fg(alert)));
            }
        });
    f.render_widget(canvas, area);
    let mapped = s.peers.list.iter().filter(|x| x.place.is_some()).count();
    let note = if mapped == 0 {
        s.peers.note.clone().or_else(|| {
            (s.peers.geo == "off" && !s.peers.list.is_empty()).then(|| {
                format!(
                    "{} peers; locations are off (--geoip off)",
                    s.peers.list.len()
                )
            })
        })
    } else {
        None
    };
    let caption =
        note.unwrap_or_else(|| format!("{mapped} peers located · {} tcp", s.peers.list.len()));
    let cap_area = Rect {
        y: area.y + area.height - 1,
        height: 1,
        ..area
    };
    f.render_widget(
        Paragraph::new(Line::styled(caption, Style::new().fg(p.dim))).alignment(Alignment::Right),
        cap_area,
    );
}

/// The block lattice: prime, region and zone lanes (rows 0, 2, 4). A block
/// of order k appears in every lane from k down to the zone, joined by `│`;
/// each lane is chained to its own previous block with `─`.
fn draw_tape(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    const GUTTER: u16 = 7;
    if area.height < 5 || area.width < GUTTER + 6 {
        return;
    }
    let tier = [p.alert, p.purple, p.fg];
    let lane = |k: usize| area.y + (k as u16) * 2;
    let ghost = app.theme == Theme::Ghost;
    let buf = f.buffer_mut();
    for (k, name) in ["PRIME", "REGION", "ZONE"].iter().enumerate() {
        buf.set_string(
            area.x,
            lane(k),
            name,
            Style::new().fg(tier[k]).add_modifier(Modifier::BOLD),
        );
    }
    let slots = ((area.width - GUTTER) / 2) as usize;
    let blocks: Vec<_> = s
        .blocks
        .iter()
        .rev()
        .take(slots)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let pad = (slots - blocks.len()) as u16 * 2;
    let x_of = |i: usize| area.x + GUTTER + pad + (i as u16) * 2;
    let fresh = app.tick.saturating_sub(app.zone_changed) < 10;
    // Chain links first, nodes over them.
    for k in 0..3usize {
        let mut prev: Option<u16> = None;
        for (i, b) in blocks.iter().enumerate() {
            if usize::from(b.order) > k {
                continue;
            }
            let x = x_of(i);
            if let Some(px) = prev {
                for xx in px + 1..x {
                    if let Some(c) = buf.cell_mut((xx, lane(k))) {
                        c.set_symbol("─")
                            .set_fg(if k == 2 { p.dim } else { tier[k] });
                    }
                }
            }
            prev = Some(x);
        }
    }
    for (i, b) in blocks.iter().enumerate() {
        let x = x_of(i);
        let newest = i + 1 == blocks.len();
        let top = usize::from(b.order.min(2));
        // Vertical link from the zone up to the block's highest tier.
        for row in (lane(top) + 1)..lane(2) {
            if row != lane(1) {
                if let Some(c) = buf.cell_mut((x, row)) {
                    c.set_symbol("│").set_fg(tier[top]);
                }
            }
        }
        for k in top..3 {
            let sym = match (k, ghost) {
                (0, true) => "◈",
                (0, false) => "▣",
                (1, _) => "◆",
                (_, true) => "●",
                _ => "■",
            };
            let ours = k == 2 && (b.ours > 0 || b.ours_block);
            let sym = match (ours, b.ours_block, ghost) {
                (true, true, _) => "★",
                (true, false, true) => "◉",
                (true, false, false) => "▩",
                _ => sym,
            };
            let color = if newest && fresh {
                p.warn
            } else if ours {
                p.ok
            } else {
                tier[k]
            };
            if let Some(c) = buf.cell_mut((x, lane(k))) {
                c.set_symbol(sym).set_fg(color);
                if newest && fresh {
                    c.set_style(Style::new().fg(color).add_modifier(Modifier::BOLD));
                }
            }
        }
    }
    // Latest prime and region numbers at the right end of their lanes.
    for (k, num) in [
        (
            0usize,
            blocks
                .iter()
                .rev()
                .find(|b| b.order == 0)
                .map(|b| b.prime_number),
        ),
        (
            1,
            blocks
                .iter()
                .rev()
                .find(|b| b.order <= 1)
                .map(|b| b.region_number),
        ),
    ] {
        if let Some(n) = num {
            let label = format!(" {} ", thousands(n));
            let x = area.x + area.width.saturating_sub(label.chars().count() as u16);
            buf.set_string(x, lane(k), &label, Style::new().fg(tier[k]).bg(p.bg));
        }
    }
}

fn draw_events(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let mut lines = Vec::new();
    for e in s.events.iter().rev().take(area.height as usize) {
        let (tag, c) = match e.kind.as_str() {
            "prime" => ("PRIME ", p.alert),
            "region" => ("REGION", p.purple),
            "reorg" => ("REORG ", p.warn),
            "stall" | "offline" | "mismatch" => ("ALERT ", p.alert),
            "resume" | "online" => ("OK    ", p.ok),
            "workshare" => ("SHARE ", p.ok),
            "mined" => ("MINED ", p.warn),
            _ => ("INFO  ", p.dim),
        };
        lines.push(Line::from(vec![
            Span::styled(utc_clock(e.t_ms), Style::new().fg(p.faint)),
            Span::raw(" "),
            Span::styled(tag, Style::new().fg(c).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(e.text.clone(), Style::new().fg(p.text)),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "watching for prime and region blocks…",
            Style::new().fg(p.dim),
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}

// ---------------------------------------------------------------- stratum

fn algo_label(a: &str) -> (&'static str, usize) {
    match a {
        "kawpow" => ("KAWPOW", 0),
        "sha256" => ("SHA", 1),
        "scrypt" => ("SCRYPT", 2),
        _ => ("?", 1),
    }
}

fn uptime(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 86_400 {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    } else {
        format!("{}h {:02}m", s / 3600, (s / 60) % 60)
    }
}

/// Seconds from a node's block timestamp to `now`, whatever the node says.
fn secs_since(now: i64, ts: u64) -> i64 {
    now.saturating_sub(i64::try_from(ts).unwrap_or(i64::MAX))
}

fn short_addr(a: &str) -> String {
    crate::state::abbrev(a, 14, 8, 4)
}

/// The node's view of its stratum, or why there is none.
fn stratum_state(s: &State) -> Result<&crate::state::Stratum, String> {
    match &s.stratum {
        None => {
            Err("no stratum API found on port 3336: set --stratum-api http://127.0.0.1:3336".into())
        }
        Some(st) if !st.online && st.workers.is_empty() => Err(format!(
            "stratum API unreachable: {}",
            st.error.clone().unwrap_or_else(|| st.api.clone())
        )),
        Some(st) => Ok(st),
    }
}

/// Overview lines shared by the panel and the full view.
fn stratum_overview<'a>(p: &Pal, st: &crate::state::Stratum) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("WORKERS ", Style::new().fg(p.dim)),
        Span::styled(
            st.workers_connected.to_string(),
            Style::new().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  MINERS ", Style::new().fg(p.dim)),
        Span::styled(
            st.miners.to_string(),
            Style::new().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  UP {}", uptime(st.uptime)),
            Style::new().fg(p.dim),
        ),
        if st.online {
            Span::raw("")
        } else {
            Span::styled("  STALE", Style::new().fg(p.alert))
        },
    ])];
    for (name, i, a) in [
        ("KAWPOW", 0, &st.kawpow),
        ("SHA", 1, &st.sha),
        ("SCRYPT", 2, &st.scrypt),
    ] {
        let c = [p.fg, p.purple, p.warn][i];
        let idle = a.workers == 0;
        lines.push(Line::from(vec![
            Span::styled(
                format!("{name:<7}"),
                Style::new()
                    .fg(if idle { p.faint } else { c })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>12}", si_rate(a.hashrate)),
                Style::new().fg(if idle { p.faint } else { p.text }),
            ),
            Span::styled(
                format!("  {:>6.3}% net", a.network_share * 100.0),
                Style::new().fg(p.dim),
            ),
            Span::styled(
                format!("  {:>6.2}/h", a.expected_per_hour),
                Style::new().fg(if idle { p.faint } else { c }),
            ),
            Span::styled(format!("  {}w", a.workers), Style::new().fg(p.dim)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("SHARES  ", Style::new().fg(p.dim)),
        Span::styled(thousands(st.shares_valid), Style::new().fg(p.ok)),
        Span::styled(" ok · ", Style::new().fg(p.dim)),
        Span::styled(
            thousands(st.shares_stale),
            Style::new().fg(if st.shares_stale > 0 { p.warn } else { p.faint }),
        ),
        Span::styled(" stale · ", Style::new().fg(p.dim)),
        Span::styled(
            thousands(st.shares_invalid),
            Style::new().fg(if st.shares_invalid > 0 {
                p.alert
            } else {
                p.faint
            }),
        ),
        Span::styled(" bad", Style::new().fg(p.dim)),
    ]));
    let oc = &st.onchain;
    lines.push(Line::from(vec![
        Span::styled("FOUND   ", Style::new().fg(p.dim)),
        Span::styled(
            thousands(st.workshares_found),
            Style::new().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" to node · chain ", Style::new().fg(p.dim)),
        Span::styled(oc.workshares.to_string(), Style::new().fg(p.ok)),
        Span::styled(" ws ", Style::new().fg(p.dim)),
        Span::styled(
            oc.blocks.to_string(),
            Style::new().fg(if oc.blocks > 0 { p.warn } else { p.faint }),
        ),
        Span::styled(format!(" blk / {}", oc.window), Style::new().fg(p.dim)),
    ]));
    if let Some(m) = st.mined {
        let n = |v: u64, c: Color| {
            Span::styled(
                v.to_string(),
                Style::new()
                    .fg(if v > 0 { c } else { p.faint })
                    .add_modifier(Modifier::BOLD),
            )
        };
        lines.push(Line::from(vec![
            Span::styled("MINED   ", Style::new().fg(p.dim)),
            Span::styled("P ", Style::new().fg(p.alert)),
            n(m.prime, p.alert),
            Span::styled(" · R ", Style::new().fg(p.purple)),
            n(m.region, p.purple),
            Span::styled(" · Z ", Style::new().fg(p.fg)),
            n(m.zone, p.fg),
            Span::styled(" blocks · paid ", Style::new().fg(p.dim)),
            n(m.workshares_paid, p.ok),
            Span::styled(format!("/{} ws", m.workshares), Style::new().fg(p.dim)),
        ]));
    }
    lines.push(if st.luck.shares == 0 {
        Line::from(vec![
            Span::styled("LUCK    ", Style::new().fg(p.dim)),
            Span::styled("— no shares in the history yet", Style::new().fg(p.faint)),
        ])
    } else {
        Line::from(vec![
            Span::styled("LUCK    ", Style::new().fg(p.dim)),
            Span::styled(
                format!("best {:.1}%", st.luck.best),
                Style::new().fg(p.text),
            ),
            Span::styled(
                format!(" · avg {:.2}% · {} shares", st.luck.average, st.luck.shares),
                Style::new().fg(p.dim),
            ),
        ])
    });
    lines
}

fn worker_line<'a>(p: &Pal, w: &crate::state::StratumWorker, now: u64, wide: bool) -> Line<'a> {
    let (algo, i) = algo_label(&w.algorithm);
    let c = [p.fg, p.purple, p.warn][i];
    let last = if w.last_share_ms == 0 {
        "—".to_string()
    } else {
        age((now.saturating_sub(w.last_share_ms) / 1000) as i64)
    };
    let mut spans = Vec::new();
    if wide {
        spans.push(Span::styled(
            format!("{:<15}", short_addr(&w.address)),
            Style::new().fg(p.dim),
        ));
    }
    spans.extend([
        Span::styled(
            format!("{:<10}", w.name.chars().take(10).collect::<String>()),
            Style::new().fg(p.text),
        ),
        Span::styled(format!("{algo:<7}"), Style::new().fg(c)),
        Span::styled(
            format!("{:>12}", si_rate(w.hashrate)),
            Style::new().fg(p.text),
        ),
        Span::styled(
            format!("  d{:<8}", compact_num(w.difficulty)),
            Style::new().fg(p.dim),
        ),
        Span::styled(format!("{:>6}", w.valid), Style::new().fg(p.ok)),
    ]);
    if wide {
        spans.push(Span::styled(
            format!(" {:>3}", w.stale),
            Style::new().fg(if w.stale > 0 { p.warn } else { p.faint }),
        ));
        spans.push(Span::styled(
            format!(" {:>3}", w.invalid),
            Style::new().fg(if w.invalid > 0 { p.alert } else { p.faint }),
        ));
    }
    spans.push(Span::styled(format!("  {last:>4}"), Style::new().fg(p.dim)));
    Line::from(spans)
}

/// `0.2`, `512`, `65.5k`.
fn compact_num(v: f64) -> String {
    if v <= 0.0 {
        "—".into()
    } else if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else if v >= 100.0 || v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// The stratum panel on the dashboard: overview and the busiest workers.
fn draw_stratum_brief(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let st = match stratum_state(s) {
        Ok(st) => st,
        Err(msg) => {
            f.render_widget(
                Paragraph::new(Line::styled(msg, Style::new().fg(p.dim)))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                area,
            );
            return;
        }
    };
    let mut lines = stratum_overview(&p, st);
    let room = (area.height as usize).saturating_sub(lines.len());
    if room > 1 && !st.workers.is_empty() {
        lines.push(Line::styled(
            "WORKER    ALGO       HASHRATE  DIFF       OK  LAST",
            Style::new().fg(p.faint),
        ));
        let now = now_ms();
        for w in st.workers.iter().take(room - 1) {
            lines.push(worker_line(&p, w, now, false));
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// `s`: every miner and worker on this node, and what became of their
/// workshares.
fn draw_mining(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let st = match stratum_state(s) {
        Ok(st) => st,
        Err(msg) => {
            let inner = panel(f, area, app, "STRATUM", "採掘");
            f.render_widget(
                Paragraph::new(vec![
                    Line::styled(msg, Style::new().fg(p.dim)),
                    Line::raw(""),
                    Line::styled(
                        "rs-quai and go-quai serve it with --node.stratum-enabled (API on :3336)",
                        Style::new().fg(p.faint),
                    ),
                ]),
                inner,
            );
            return;
        }
    };
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(area);
    let over_h = 9.min(left.height);
    let [over, miners] =
        Layout::vertical([Constraint::Length(over_h), Constraint::Min(0)]).areas(left);
    let inner = panel(f, over, app, "STRATUM", "採掘");
    f.render_widget(Paragraph::new(stratum_overview(&p, st)), inner);
    // Miners: workers and what the chain paid them in the window.
    let inner = panel(f, miners, app, "MINERS", "鉱夫");
    let mut lines = vec![Line::styled(
        format!(
            "{:<15} {:>3}  {:<14} {:>5} {:>4}",
            "ADDRESS", "WK", "ALGORITHMS", "WS", "BLK"
        ),
        Style::new().fg(p.faint),
    )];
    let mut by = st.onchain.by_address.clone();
    by.sort_by(|a, b| {
        (b.workshares + b.blocks * 100)
            .cmp(&(a.workshares + a.blocks * 100))
            .then(b.workers.cmp(&a.workers))
    });
    for a in by.iter().take((inner.height as usize).saturating_sub(3)) {
        let algos = a
            .algorithms
            .iter()
            .map(|x| algo_label(x).0)
            .collect::<Vec<_>>()
            .join("+");
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<15}", short_addr(&a.address)),
                Style::new().fg(p.text),
            ),
            Span::styled(format!(" {:>3}", a.workers), Style::new().fg(p.dim)),
            Span::styled(format!("  {algos:<14}"), Style::new().fg(p.fg)),
            Span::styled(
                format!(" {:>5}", a.workshares),
                Style::new().fg(if a.workshares > 0 { p.ok } else { p.faint }),
            ),
            Span::styled(
                format!(" {:>4}", a.blocks),
                Style::new().fg(if a.blocks > 0 { p.warn } else { p.faint }),
            ),
        ]));
    }
    lines.push(Line::styled(
        format!("paid in the last {} canonical blocks", st.onchain.window),
        Style::new().fg(p.dim),
    ));
    lines.push(Line::styled(
        format!(
            "handed to the node: {} included, {} became blocks",
            st.onchain.found_included, st.onchain.found_blocks
        ),
        Style::new().fg(p.dim),
    ));
    f.render_widget(Paragraph::new(lines), inner);
    // Workers, then the workshares handed to the node.
    let found_h = (right.height / 3).clamp(4, 14).min(right.height);
    let [workers, found] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(found_h)]).areas(right);
    let inner = panel(f, workers, app, "WORKERS", "作業");
    let wide = inner.width >= 82;
    let head = if wide {
        "ADDRESS        WORKER    ALGO       HASHRATE  DIFF       OK  ST BAD  LAST"
    } else {
        "WORKER    ALGO       HASHRATE  DIFF       OK  LAST"
    };
    let mut lines = vec![Line::styled(head, Style::new().fg(p.faint))];
    let now = now_ms();
    let room = (inner.height as usize).saturating_sub(1);
    let shown = st.workers.len().min(room);
    let hidden = st.workers.len() - shown;
    for w in st.workers.iter().take(if hidden > 0 {
        shown.saturating_sub(1)
    } else {
        shown
    }) {
        lines.push(worker_line(&p, w, now, wide));
    }
    if hidden > 0 {
        lines.push(Line::styled(
            format!("… {} more workers", hidden + 1),
            Style::new().fg(p.dim),
        ));
    }
    if st.workers.is_empty() {
        lines.push(Line::styled("no workers connected", Style::new().fg(p.dim)));
    }
    f.render_widget(Paragraph::new(lines), inner);
    let inner = panel(f, found, app, "HANDED TO NODE", "提出");
    let mut lines = Vec::new();
    for x in st.found.iter().take(inner.height as usize) {
        let (algo, i) = algo_label(&x.algorithm);
        let (tag, c) = match x.status.as_str() {
            "block" => ("BLOCK   ", p.warn),
            "included" => ("INCLUDED", p.ok),
            "unseen" => ("OLDER   ", p.faint),
            _ => ("PENDING ", p.dim),
        };
        lines.push(Line::from(vec![
            Span::styled(utc_clock(x.found_ms), Style::new().fg(p.faint)),
            Span::styled(
                format!(" {tag} "),
                Style::new().fg(c).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{} ", thousands(x.height)), Style::new().fg(p.text)),
            Span::styled(
                format!("{algo:<7}"),
                Style::new().fg([p.fg, p.purple, p.warn][i]),
            ),
            Span::styled(
                crate::collect::short_worker(&x.worker),
                Style::new().fg(p.dim),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "no workshares yet: shares that meet the workshare target appear here",
            Style::new().fg(p.dim),
        ));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_logs(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    if s.logs.is_empty() {
        let msg = match &s.node.log_file {
            Some(file) => format!("following {file}"),
            None => {
                "no log file found: run as the node's user, or set --logs <nodelogs dir or file>"
                    .into()
            }
        };
        f.render_widget(
            Paragraph::new(Line::styled(msg, Style::new().fg(p.dim))),
            area,
        );
        return;
    }
    let n = area.height as usize;
    let start = s.logs.len().saturating_sub(n);
    let lines: Vec<Line> = s
        .logs
        .iter()
        .skip(start)
        .map(|l| {
            let c = match l.level.as_str() {
                "ERROR" => p.alert,
                "WARN" => p.warn,
                "DEBUG" | "TRACE" => p.faint,
                _ => p.text,
            };
            let text: String = l.text.chars().take(area.width as usize).collect();
            Line::styled(text, Style::new().fg(c))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_flash(f: &mut Frame, app: &App, fl: &Flash, area: Rect) {
    let p = pal(app.theme);
    let left = fl.until.saturating_sub(app.tick);
    match app.theme {
        Theme::Ghost => {
            let title = match fl.kind {
                Moment::Prime => "PRIME CONVERGENCE // 主鎖収束",
                Moment::Region => "REGION CONVERGENCE // 領域収束",
                Moment::Mined => "BLOCK ACQUIRED // 採掘成功",
            };
            let cols = |t: &str, pad: u16| {
                u16::try_from(t.chars().count())
                    .unwrap_or(u16::MAX)
                    .saturating_add(pad)
            };
            let w = cols(title, 10).max(cols(&fl.text, 6)).min(area.width);
            let r = Rect {
                x: area.x + (area.width - w) / 2,
                y: area.y + (area.height / 2).saturating_sub(2),
                width: w,
                height: 5.min(area.height),
            };
            let on = left % 4 < 2;
            let hue = if fl.kind == Moment::Mined { p.ok } else { p.fg };
            let (fg, bg) = if on { (p.bg, hue) } else { (hue, p.bg) };
            f.render_widget(Clear, r);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(hue))
                .style(Style::new().bg(bg));
            let text = vec![
                Line::styled(title, Style::new().fg(fg).add_modifier(Modifier::BOLD)),
                Line::styled(fl.text.clone(), Style::new().fg(fg)),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .block(block)
                    .alignment(Alignment::Center),
                r,
            );
        }
        Theme::Angel => {
            let r = Rect {
                x: area.x,
                y: area.y + (area.height / 2).saturating_sub(3),
                width: area.width,
                height: 6.min(area.height),
            };
            let on = left % 4 < 2;
            let bg = match fl.kind {
                Moment::Prime => p.alert,
                Moment::Region => p.purple,
                Moment::Mined => p.ok,
            };
            f.render_widget(Clear, r);
            let stripe: String = (0..area.width)
                .map(|i| {
                    if ((i as u64 + app.tick) / 3) % 2 == 0 {
                        '▞'
                    } else {
                        ' '
                    }
                })
                .collect();
            let title = match fl.kind {
                Moment::Prime => "PATTERN PRIME — 主鎖確認",
                Moment::Region => "PATTERN REGION — 領域確認",
                Moment::Mined => "PATTERN MINED — 採掘確認",
            };
            let text = vec![
                Line::styled(stripe.clone(), Style::new().fg(Color::Black).bg(bg)),
                Line::raw(""),
                Line::styled(
                    title,
                    Style::new()
                        .fg(if on { Color::White } else { Color::Black })
                        .add_modifier(Modifier::BOLD),
                ),
                Line::styled(fl.text.to_uppercase(), Style::new().fg(Color::Black)),
                Line::raw(""),
                Line::styled(stripe, Style::new().fg(Color::Black).bg(bg)),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .style(Style::new().bg(bg))
                    .alignment(Alignment::Center),
                r,
            );
        }
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let p = pal(app.theme);
    let w = 46.min(area.width);
    let h = 12.min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let inner = panel(f, r, app, "CONTROLS", "操作");
    let lines = vec![
        kv(&p, "t", "toggle GHOST / ANGEL".into(), p.text),
        kv(&p, "m", "full-screen peer map".into(), p.text),
        kv(&p, "l", "full-height node log".into(), p.text),
        kv(&p, "s", "stratum: miners on this node".into(), p.text),
        kv(&p, "? / h", "this help".into(), p.text),
        kv(&p, "q / Esc", "quit".into(), p.text),
        Line::raw(""),
        Line::styled("works with rs-quai and go-quai", Style::new().fg(p.dim)),
    ];
    f.render_widget(Paragraph::new(lines).style(Style::new().bg(p.bg)), inner);
}

fn draw_boot(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let steps: [&str; 6] = match app.theme {
        Theme::Ghost => [
            "電脳接続 // CYBERBRAIN LINK",
            "> handshake  quai json-rpc",
            "> mount      prime / region / zone",
            "> calibrate  kawpow · sha · scrypt",
            "> trace      peer constellation",
            "DIVE",
        ],
        Theme::Angel => [
            "SYSTEM START — 起動",
            "POWER         EXTERNAL",
            "LINK          PRIME · REGION · ZONE",
            "TRIAD         KAWPOW · SHA · SCRYPT",
            "HARMONICS     NORMAL",
            "ALL SYSTEMS 承認",
        ],
    };
    let shown = ((app.tick as usize) * steps.len() / BOOT_TICKS as usize + 1).min(steps.len());
    let mut lines: Vec<Line> = Vec::new();
    for (i, st) in steps.iter().take(shown).enumerate() {
        let last = i + 1 == steps.len();
        let style = if last {
            Style::new().fg(p.bg).bg(p.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(p.fg)
        };
        lines.push(Line::styled(format!(" {st} "), style));
    }
    let w = 50.min(area.width) as usize;
    let done = (app.tick as usize * w) / BOOT_TICKS as usize;
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("█".repeat(done.min(w)), Style::new().fg(p.fg)),
        Span::styled("░".repeat(w.saturating_sub(done)), Style::new().fg(p.faint)),
    ]));
    lines.push(Line::styled(
        format!("{}  {}", s.node.label, s.node.rpc),
        Style::new().fg(p.dim),
    ));
    let h = lines.len() as u16;
    let r = Rect {
        y: area.y + area.height.saturating_sub(h) / 2,
        height: h.min(area.height),
        ..area
    };
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), r);
}

/// A faint horizontal band sweeping down the screen (GHOST).
fn scanline(f: &mut Frame, app: &App, area: Rect) {
    let p = pal(app.theme);
    if area.height == 0 {
        return;
    }
    let y = area.y + ((app.tick / 2) % area.height as u64) as u16;
    let buf = f.buffer_mut();
    for x in area.x..area.x + area.width {
        if let Some(c) = buf.cell_mut((x, y)) {
            if c.bg == p.bg || c.bg == Color::Reset {
                c.set_bg(p.scan);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BlockInfo, Compare, Head, Mining, Peer, Place};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample() -> State {
        let now = now_ms();
        let mut s = State::default();
        s.node.label = "RS-QUAI SOAK".into();
        s.node.rpc = "http://127.0.0.1:9200".into();
        s.node.kind = "rs-quai".into();
        s.node.location = "Cyprus-1".into();
        s.node.chain_id = Some(9);
        s.node.online = true;
        let t = now / 1000;
        s.chains.prime = Some(Head {
            number: 2_297_888,
            hash: "0xaa".into(),
            timestamp: t - 40,
        });
        s.chains.region = Some(Head {
            number: 5_579_748,
            hash: "0xbb".into(),
            timestamp: t - 12,
        });
        s.chains.zone = Some(Head {
            number: 10_390_405,
            hash: "0x435ba623ce10a380".into(),
            timestamp: t - 3,
        });
        for i in 0..60u64 {
            s.blocks.push_back(BlockInfo {
                number: 10_390_346 + i,
                hash: format!("0x{i:064x}"),
                timestamp: t - 300 + i * 5,
                txs: (i % 7) as u32,
                workshares: (i % 12) as u32,
                gas_used: (i * 700_000) % 50_000_000,
                gas_limit: 50_000_000,
                base_fee: "27231908540241".into(),
                order: if i == 40 {
                    0
                } else if i % 9 == 0 {
                    1
                } else {
                    2
                },
                difficulty: "1038556905100".into(),
                exchange_rate: "13264669140000000000".into(),
                ..Default::default()
            });
        }
        s.mining = Some(Mining {
            avg_block_time: 5.257,
            blocks_analyzed: 172,
            kawpow: Algo {
                hashrate: 243_688_736_202.0,
                difficulty: "1".into(),
                share_time: 5.2,
            },
            sha: Algo {
                hashrate: 2.56e17,
                difficulty: "1".into(),
                share_time: 1.27,
            },
            scrypt: Algo {
                hashrate: 0.0,
                difficulty: "1".into(),
                share_time: 0.0,
            },
            estimated_block_reward: "105.3384".into(),
            workshare_reward: "11.7042".into(),
            quai_supply: "1124728606".into(),
            ..Default::default()
        });
        s.pending_shares.kawpow = 1;
        s.pending_shares.sha = 8;
        s.peers.count = 85;
        s.peers.inbound = 76;
        s.peers.outbound = 9;
        s.peers.geo = "db".into();
        s.peers.here = Some(Place {
            lat: 41.9,
            lon: -87.6,
            city: String::new(),
            country: "US".into(),
        });
        for (lat, lon) in [
            (52.5, 13.4),
            (35.7, 139.7),
            (-33.9, 151.2),
            (40.7, -74.0),
            (1.35, 103.8),
        ] {
            s.peers.list.push(Peer {
                ip: "203.0.113.7".into(),
                port: 4002,
                dir: "in".into(),
                place: Some(Place {
                    lat,
                    lon,
                    city: String::new(),
                    country: String::new(),
                }),
                since_ms: now,
            });
        }
        s.compare = Some(Compare {
            label: "GO-QUAI".into(),
            rpc: "x".into(),
            height: 10_390_405,
            online: true,
            compared: 64,
            matched: 64,
            last_mismatch: None,
        });
        s.push_event(
            now - 5000,
            "prime",
            "Prime block 2297888 (zone 10390386)".into(),
            Some(10_390_386),
        );
        s.push_event(
            now - 2000,
            "region",
            "Region block 5579748 (zone 10390400)".into(),
            Some(10_390_400),
        );
        s.push_log(
            "INFO".into(),
            "2026-10-01T17:23:03Z  INFO rsq_node::node: network peers=85".into(),
        );
        s.push_log("WARN".into(), "2026-10-01T17:23:04Z  WARN rsq_chain::indexer: ChainIndexer: Reorging the utxo indexer len=1".into());
        s
    }

    fn render(theme: Theme, w: u16, h: u16, view: View) -> String {
        let mut term = match Terminal::new(TestBackend::new(w, h)) {
            Ok(t) => t,
            Err(e) => return format!("terminal error {e}"),
        };
        let s = sample();
        let mut app = App::new(theme);
        app.tick = BOOT_TICKS + 3;
        app.view = view;
        app.observe(&s);
        app.hist = [
            VecDeque::from(vec![1100, 1110, 1105]),
            VecDeque::from(vec![1740, 1741]),
            VecDeque::new(),
        ];
        if term.draw(|f| draw(f, &app, &s)).is_err() {
            return "draw error".into();
        }
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        let wide = |c: char| matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFF00..=0xFF60);
        for y in 0..buf.area.height {
            let mut x = 0;
            while x < buf.area.width {
                let sym = buf.cell((x, y)).map_or(" ", |c| c.symbol());
                out.push_str(sym);
                // A double-width glyph occupies the next cell too.
                x += if sym.chars().next().is_some_and(wide) {
                    2
                } else {
                    1
                };
            }
            out.push('\n');
        }
        if std::env::var("QUAI_DASH_SHOW").is_ok() {
            println!("{out}");
        }
        out
    }

    #[test]
    fn renders_both_themes() {
        let g = render(Theme::Ghost, 160, 48, View::Dash);
        for want in [
            "QUAI//DIVE",
            "RS-QUAI SOAK",
            "◇ RS-QUAI",
            "Cyprus-1",
            "HIERARCHY",
            "MERGED MINING",
            "243.69 GH/s",
            "256.00 PH/s",
            "PEER MAP",
            "NODE LOG",
            "network peers=85",
            "100.0%",
            "BLOCK LATTICE",
        ] {
            assert!(g.contains(want), "GHOST frame lacks {want:?}\n{g}");
        }
        let a = render(Theme::Angel, 160, 48, View::Dash);
        for want in [
            "QUAI TERMINAL",
            "KAWPOW·1",
            "SHA·2",
            "SCRYPT·3",
            "承認 APPROVED",
            "否決 DENIED",
            "ECONOMY",
            "105.3384 QUAI",
            "27,231 Gwei",
            "13.2646",
        ] {
            assert!(a.contains(want), "ANGEL frame lacks {want:?}\n{a}");
        }
        let small = render(Theme::Angel, 90, 26, View::Dash);
        assert!(
            small.contains("ZONE HEIGHT") && small.contains("NODE LOG"),
            "{small}"
        );
        let map = render(Theme::Ghost, 120, 40, View::Map);
        assert!(map.contains("5 peers located"), "{map}");
        let logs = render(Theme::Ghost, 100, 30, View::Logs);
        assert!(logs.contains("Reorging the utxo indexer"), "{logs}");
    }

    #[test]
    fn overlays_and_tiny_terminals() {
        let s = sample();
        for theme in [Theme::Ghost, Theme::Angel] {
            for (w, h) in [(160, 48), (40, 10), (8, 3), (1, 1)] {
                let Ok(mut term) = Terminal::new(TestBackend::new(w, h));
                for tick in [0, 5, BOOT_TICKS + 1] {
                    let mut app = App::new(theme);
                    app.tick = tick;
                    app.help = true;
                    app.flash = Some(Flash {
                        until: tick + 5,
                        kind: [Moment::Prime, Moment::Region, Moment::Mined][(tick % 3) as usize],
                        text: "Prime block 1 (zone 2)".into(),
                    });
                    let mut offline = s.clone();
                    offline.node.online = tick == 0;
                    offline.node.error = Some("connection refused".into());
                    assert!(term.draw(|f| draw(f, &app, &offline)).is_ok());
                }
            }
        }
    }

    fn with_stratum(mut s: State) -> State {
        use crate::state::{AddressPaid, FoundShare, OnChain, Stratum, StratumAlgo, StratumWorker};
        let w = |addr: &str, name: &str, algo: &str, h: f64, d: f64, ok: u64| StratumWorker {
            address: addr.into(),
            name: name.into(),
            algorithm: algo.into(),
            difficulty: d,
            hashrate: h,
            valid: ok,
            stale: 1,
            invalid: 0,
            last_share_ms: now_ms() - 12_000,
            connected_ms: now_ms() - 3_600_000,
        };
        let a = "0x00051234AbCdEf0123456789aBcDeF01234567Fe";
        let b = "0x00771a5e0b3c9d24e6f8a1b2c3d4e5f60718293a";
        s.stratum = Some(Stratum {
            api: "http://127.0.0.1:3336".into(),
            online: true,
            uptime: 7_380.0,
            workers_connected: 3,
            workers_total: 4,
            miners: 2,
            shares_valid: 1_234,
            shares_stale: 5,
            shares_invalid: 2,
            workshares_found: 17,
            kawpow: StratumAlgo {
                hashrate: 3.04e7,
                workers: 1,
                shares_valid: 300,
                network_share: 1.0e-4,
                expected_per_hour: 0.072,
            },
            sha: StratumAlgo {
                hashrate: 4.58e14,
                workers: 2,
                shares_valid: 934,
                network_share: 1.8e-3,
                expected_per_hour: 5.4,
            },
            workers: vec![
                w(b, "s21-01", "sha256", 2.31e14, 65536.0, 470),
                w(b, "s21-02", "sha256", 2.27e14, 65536.0, 464),
                w(a, "gpu0", "kawpow", 3.04e7, 0.2, 300),
            ],
            found: vec![
                FoundShare {
                    height: 10_390_404,
                    hash: "0xb2".into(),
                    worker: format!("{b}.s21-01"),
                    algorithm: "sha256".into(),
                    found_ms: now_ms() - 20_000,
                    status: "block".into(),
                },
                FoundShare {
                    height: 10_390_401,
                    hash: "0xa1".into(),
                    worker: format!("{a}.gpu0"),
                    algorithm: "kawpow".into(),
                    found_ms: now_ms() - 90_000,
                    status: "included".into(),
                },
            ],
            onchain: OnChain {
                window: 96,
                workshares: 6,
                blocks: 1,
                found_included: 1,
                found_blocks: 1,
                by_address: vec![
                    AddressPaid {
                        address: a.into(),
                        workers: 1,
                        algorithms: vec!["kawpow".into()],
                        workshares: 1,
                        blocks: 0,
                    },
                    AddressPaid {
                        address: b.into(),
                        workers: 2,
                        algorithms: vec!["sha256".into()],
                        workshares: 5,
                        blocks: 1,
                    },
                ],
            },
            ..Default::default()
        });
        s
    }

    fn render_state(s: &State, theme: Theme, w: u16, h: u16, view: View) -> String {
        let Ok(mut term) = Terminal::new(TestBackend::new(w, h));
        let mut app = App::new(theme);
        app.tick = BOOT_TICKS + 3;
        app.view = view;
        app.observe(s);
        if term.draw(|f| draw(f, &app, s)).is_err() {
            return "draw error".into();
        }
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            let mut x = 0;
            while x < buf.area.width {
                let sym = buf.cell((x, y)).map_or(" ", |c| c.symbol());
                out.push_str(sym);
                x += if sym.chars().next().is_some_and(wide) {
                    2
                } else {
                    1
                };
            }
            out.push('\n');
        }
        if std::env::var("QUAI_DASH_SHOW").is_ok() {
            println!("{out}");
        }
        out
    }

    #[test]
    fn renders_the_stratum() {
        let s = with_stratum(sample());
        for theme in [Theme::Ghost, Theme::Angel] {
            let d = render_state(&s, theme, 160, 48, View::Dash);
            for want in [
                "STRATUM",
                "WORKERS 3",
                "MINERS 2",
                "s21-01",
                "1,234",
                "s stratum",
            ] {
                assert!(d.contains(want), "dashboard lacks {want:?}\n{d}");
            }
            let m = render_state(&s, theme, 160, 48, View::Mining);
            for want in [
                "MINERS",
                "HANDED TO NODE",
                "BLOCK",
                "INCLUDED",
                "gpu0",
                "0x000512…67Fe",
                "KAWPOW",
                "30.40 MH/s",
                "d0.2",
                "paid in the last 96 canonical blocks",
                "1 included, 1 became blocks",
            ] {
                assert!(m.contains(want), "mining view lacks {want:?}\n{m}");
            }
        }
        // Not configured: the view says how, the dashboard keeps its layout.
        let bare = sample();
        let m = render_state(&bare, Theme::Ghost, 120, 40, View::Mining);
        assert!(m.contains("--stratum-api"), "{m}");
        let d = render_state(&bare, Theme::Ghost, 160, 48, View::Dash);
        assert!(!d.contains("STRATUM") && d.contains("EVENTS"), "{d}");
        // Unreachable.
        let mut down = with_stratum(sample());
        if let Some(st) = down.stratum.as_mut() {
            st.online = false;
            st.workers.clear();
            st.error = Some("connection refused".into());
        }
        let d = render_state(&down, Theme::Angel, 160, 48, View::Dash);
        assert!(d.contains("unreachable"), "{d}");
        // A mined block takes the screen.
        let mut s2 = with_stratum(sample());
        let mut app = App::new(Theme::Ghost);
        app.tick = BOOT_TICKS + 3;
        app.observe(&s2);
        s2.push_event(
            now_ms() + 10,
            "mined",
            "Block 10390405 mined through this node by 0x000512…67Fe.gpu0 (kawpow)".into(),
            Some(10_390_405),
        );
        app.observe(&s2);
        assert!(app.flash.as_ref().is_some_and(|f| f.kind == Moment::Mined));
        for (w, h) in [(160, 48), (40, 10), (8, 3)] {
            let Ok(mut term) = Terminal::new(TestBackend::new(w, h));
            for theme in [Theme::Ghost, Theme::Angel] {
                app.theme = theme;
                app.view = View::Mining;
                assert!(term.draw(|f| draw(f, &app, &s2)).is_ok());
            }
        }
    }

    #[test]
    fn hostile_numbers_and_names_draw() {
        let mut s = with_stratum(sample());
        for h in [
            &mut s.chains.prime,
            &mut s.chains.region,
            &mut s.chains.zone,
        ] {
            if let Some(h) = h.as_mut() {
                h.timestamp = u64::MAX;
                h.number = u64::MAX;
            }
        }
        s.peers.inbound = u32::MAX;
        s.peers.outbound = u32::MAX;
        if let Some(st) = s.stratum.as_mut() {
            for w in &mut st.workers {
                w.address = "0x€€€€€€€€€€€€€€€€€€€€".into();
                w.name = "é".repeat(300);
            }
        }
        let mut app = App::new(Theme::Ghost);
        app.tick = BOOT_TICKS + 3;
        app.observe(&s);
        // 65531 bytes: `len() as u16 + 6` used to overflow.
        s.push_event(now_ms() + 10, "mined", "x".repeat(65_531), Some(1));
        app.observe(&s);
        let Ok(mut term) = Terminal::new(TestBackend::new(160, 48));
        for theme in [Theme::Ghost, Theme::Angel] {
            for view in [View::Dash, View::Logs, View::Map, View::Mining] {
                app.theme = theme;
                app.view = view;
                assert!(term.draw(|f| draw(f, &app, &s)).is_ok());
            }
        }
    }

    #[test]
    fn stuck_state_shows_stale() {
        let mut s = sample();
        s.now_ms = now_ms();
        assert!(!render_state(&s, Theme::Ghost, 160, 48, View::Dash).contains("STALE"));
        s.now_ms = now_ms() - 30_000;
        assert!(render_state(&s, Theme::Ghost, 160, 48, View::Dash).contains("STALE"));
    }

    #[test]
    fn glyph_fonts() {
        let r = big_rows("10,4");
        assert_eq!(r[0], " █  ███   █ █ ");
        let s = seg_rows("8.5");
        assert_eq!(s[0], " ━━     ━━  ");
        assert_eq!(si_rate(2.56e17), "256.00 PH/s");
        assert_eq!(thousands(10_390_405), "10,390,405");
        assert_eq!(e18("13264669140000000000"), "13.2646");
    }
}
