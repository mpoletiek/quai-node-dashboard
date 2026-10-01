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
}

struct Flash {
    until: u64,
    prime: bool,
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
            if e.kind == "prime" || e.kind == "region" {
                self.flash = Some(Flash { until: self.tick + FLASH_TICKS, prime: e.kind == "prime", text: e.text.clone() });
            }
        }
        if let Some(e) = s.events.back() {
            self.last_event_ms = self.last_event_ms.max(e.t_ms);
        }
        if let Some(m) = &s.mining {
            let key = format!("{}/{}/{}", m.kawpow.hashrate, m.sha.hashrate, m.scrypt.hashrate);
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
                self.theme = if self.theme == Theme::Ghost { Theme::Angel } else { Theme::Ghost };
            }
            KeyCode::Char('l') => self.view = if self.view == View::Logs { View::Dash } else { View::Logs },
            KeyCode::Char('m') => self.view = if self.view == View::Map { View::Dash } else { View::Map },
            KeyCode::Char('?') | KeyCode::Char('h') => self.help = !self.help,
            _ => {}
        }
        false
    }

    fn rand(&self, salt: u64) -> u64 {
        let mut x = self.tick.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 29)
    }
}

/// Hashrate as a sparkline value (three significant digits of its decade).
fn scale_rate(h: f64) -> u64 {
    if h <= 0.0 { 0 } else { (h.log10() * 100.0).max(0.0) as u64 }
}

/// Runs until the user quits.
pub fn run(state: Arc<Mutex<State>>, theme: Theme) -> Result<(), String> {
    let mut terminal = ratatui::try_init().map_err(|e| format!("terminal: {e}"))?;
    let result = event_loop(&mut terminal, &state, theme);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, state: &Arc<Mutex<State>>, theme: Theme) -> Result<(), String> {
    let mut app = App::new(theme);
    loop {
        let snap = state.lock().map(|s| s.clone()).map_err(|_| "state lock poisoned".to_string())?;
        app.observe(&snap);
        terminal.draw(|f| draw(f, &app, &snap)).map_err(|e| e.to_string())?;
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
        let snap = state.lock().map(|s| s.clone()).map_err(|_| "state lock poisoned".to_string())?;
        app.observe(&snap);
        term.draw(|f| draw(f, &app, &snap)).map_err(|e| e.to_string())?;
        if t % every.max(1) == 0 {
            let buf = term.backend().buffer().clone();
            let mut runs: Vec<serde_json::Value> = Vec::new();
            for y in 0..h {
                let mut x = 0u16;
                while x < w {
                    let changed = |xx: u16| prev.as_ref().is_none_or(|p| p.cell((xx, y)) != buf.cell((xx, y)));
                    if !changed(x) {
                        x += 1;
                        continue;
                    }
                    let Some(c0) = buf.cell((x, y)) else { break };
                    let st = c0.style();
                    let mut flags = 0u8;
                    let m = st.add_modifier;
                    if m.contains(Modifier::BOLD) { flags |= 1; }
                    if m.contains(Modifier::DIM) { flags |= 2; }
                    if m.contains(Modifier::ITALIC) { flags |= 4; }
                    if m.contains(Modifier::REVERSED) { flags |= 8; }
                    if m.contains(Modifier::UNDERLINED) { flags |= 16; }
                    let key = format!("{}|{}|{flags}", hex_color(st.fg.unwrap_or(Color::Reset), "#d0d0d0"), hex_color(st.bg.unwrap_or(Color::Reset), "#000000"));
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
                        x += if sym.chars().next().is_some_and(wide) { 2 } else { 1 };
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
    Ok(serde_json::json!({"w": w, "h": h, "frame_ms": ms, "styles": styles, "frames": frames, "captions": captions}))
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
    if h.len() > 14 { format!("{}…{}", &h[..8], &h[h.len() - 4..]) } else { h.to_string() }
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
            let v = |l: bool, r: bool| format!("{}  {}", if l { "┃" } else { " " }, if r { "┃" } else { " " });
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
                Span::styled(title.to_string(), Style::new().fg(p.fg).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {jp} "), Style::new().fg(p.dim)),
            ])),
        Theme::Angel => Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::new().fg(p.dim))
            .title(Line::from(vec![
                Span::styled(format!(" {title} "), Style::new().fg(Color::Black).bg(p.fg).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {jp} "), Style::new().fg(p.warn)),
            ])),
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.theme == Theme::Ghost && area.width > 4 && area.height > 2 {
        // Bright corner ticks over the faint frame.
        let (l, r, t, b) = (area.x, area.x + area.width - 1, area.y, area.y + area.height - 1);
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
    Line::from(vec![Span::styled(format!("{k:<11}"), Style::new().fg(p.dim)), Span::styled(v, Style::new().fg(vc))])
}

// ---------------------------------------------------------------- draw

fn draw(f: &mut Frame, app: &App, s: &State) {
    let p = pal(app.theme);
    let area = f.area();
    f.render_widget(Block::default().style(Style::new().bg(p.bg).fg(p.text)), area);
    if app.tick < BOOT_TICKS {
        draw_boot(f, app, s, area);
        return;
    }
    let header_h = if app.theme == Theme::Angel { 3 } else { 2 };
    let [head, body] = Layout::vertical([Constraint::Length(header_h), Constraint::Min(0)]).areas(area);
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
        let [top, algos, logs] =
            Layout::vertical([Constraint::Length(9), Constraint::Length(9), Constraint::Min(4)]).areas(area);
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
    let [hero, chains, side] =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(27), Constraint::Percentage(27)]).areas(top);
    let inner = panel(f, hero, app, "ZONE HEIGHT", "鎖高");
    draw_hero(f, app, s, inner);
    let inner = panel(f, chains, app, "HIERARCHY", "階層");
    draw_chains(f, app, s, inner);
    let [peers, econ] = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(side);
    let inner = panel(f, peers, app, "PEERS", "接続");
    draw_peers(f, app, s, inner);
    let inner = panel(f, econ, app, "ECONOMY", "経済");
    draw_econ(f, app, s, inner);

    let [algos, map] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(mid);
    draw_algos(f, app, s, algos);
    let inner = panel(f, map, app, "PEER MAP", "地図");
    draw_map(f, app, s, inner);

    if tape_h > 0 {
        let inner = panel(f, tape, app, "BLOCK LATTICE", "階層");
        draw_tape(f, app, s, inner);
    }
    let [events, logs] = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(bottom);
    let inner = panel(f, events, app, "EVENTS", "事象");
    draw_events(f, app, s, inner);
    let inner = panel(f, logs, app, "NODE LOG", "記録");
    draw_logs(f, app, s, inner);
}

fn draw_header(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let now = now_ms();
    let live = if s.node.online {
        Span::styled(" ● LIVE ", Style::new().fg(Color::Black).bg(p.ok).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" ✕ OFFLINE ", Style::new().fg(Color::White).bg(p.alert).add_modifier(Modifier::BOLD))
    };
    let location = if s.node.location.is_empty() { "—".to_string() } else { s.node.location.clone() };
    let chain = s.node.chain_id.map_or("—".to_string(), |c| c.to_string());
    let (brand, theme_name) = match app.theme {
        Theme::Ghost => ("QUAI//DIVE", "GHOST"),
        Theme::Angel => ("QUAI TERMINAL", "ANGEL"),
    };
    let info = Line::from(vec![
        Span::styled(format!(" {brand} "), Style::new().fg(p.bg).bg(p.fg).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(s.node.label.clone(), Style::new().fg(p.text).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ◇ {location}  ◇ CHAIN {chain}  "), Style::new().fg(p.dim)),
        live,
        Span::styled(format!("  {theme_name} ", ), Style::new().fg(p.purple)),
        Span::styled(utc_clock(now), Style::new().fg(p.fg)),
        Span::styled("   t theme · m map · l log · ? help", Style::new().fg(p.faint)),
    ]);
    match app.theme {
        Theme::Ghost => {
            let rule: String = (0..area.width)
                .map(|i| if (i as u64 + app.tick / 2) % 24 == 0 { '╸' } else { '─' })
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
            Line::styled(title, Style::new().fg(if blink { p.alert } else { p.warn }).add_modifier(Modifier::BOLD)),
            Line::raw(""),
            Line::styled(format!("RPC {}", s.node.rpc), Style::new().fg(p.text)),
            Line::styled(err, Style::new().fg(p.alert)),
        ];
        f.render_widget(Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }), area);
        return;
    }
    let zone = s.chains.zone.as_ref();
    let number = zone.map_or(0, |z| z.number);
    let since = zone.map_or(0.0, |z| (now_ms() as f64 / 1000.0 - z.timestamp as f64).max(0.0));
    let [left, right] = Layout::horizontal([Constraint::Min(30), Constraint::Length(24)]).areas(area);
    // Big height, scrambled for a moment after it changes (GHOST).
    let rows = big_rows(&group_digits(number));
    let fresh = app.tick.saturating_sub(app.zone_changed) < 4 && app.zone_changed > 0;
    let mut lines: Vec<Line> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let style = Style::new().fg(if fresh && app.theme == Theme::Angel { p.warn } else { p.fg }).add_modifier(Modifier::BOLD);
        if fresh && app.theme == Theme::Ghost {
            const KANA: [char; 12] = ['ア', 'カ', 'サ', 'タ', 'ナ', 'ハ', 'マ', 'ヤ', 'ラ', 'ワ', 'ン', 'ヲ'];
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
        Span::styled(last.map_or("—".into(), |b| b.txs.to_string()), Style::new().fg(p.text)),
        Span::styled("  WS ", Style::new().fg(p.dim)),
        Span::styled(last.map_or("—".into(), |b| b.workshares.to_string()), Style::new().fg(p.text)),
        Span::styled("  ", Style::new()),
        Span::styled(last.map_or(String::new(), |b| short_hash(&b.hash)), Style::new().fg(p.faint)),
    ]));
    f.render_widget(Paragraph::new(lines), left);
    // Seven-segment timer: seconds since the last block.
    let late = avg > 0.0 && since > avg * 3.0;
    let timer = if since >= 100.0 { format!("{:03}", since as u64) } else { format!("{since:04.1}") };
    let label = match app.theme {
        Theme::Ghost => "SINCE LAST BLOCK",
        Theme::Angel => "活動限界 BLOCK TIMER",
    };
    let color = if late { p.alert } else if app.theme == Theme::Angel { p.warn } else { p.fg };
    let mut tl = vec![Line::styled(label, Style::new().fg(p.dim))];
    for row in seg_rows(&timer) {
        tl.push(Line::styled(row, Style::new().fg(color).add_modifier(Modifier::BOLD)));
    }
    let gauge_w = right.width.saturating_sub(2) as f64;
    let frac = if avg > 0.0 { (since / (avg * 2.0)).min(1.0) } else { 0.0 };
    let filled = (gauge_w * frac) as usize;
    tl.push(Line::from(vec![
        Span::styled("▮".repeat(filled), Style::new().fg(color)),
        Span::styled("▯".repeat((gauge_w as usize).saturating_sub(filled)), Style::new().fg(p.faint)),
    ]));
    f.render_widget(Paragraph::new(tl), right);
}

fn draw_chains(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let now = now_ms() as i64 / 1000;
    let mut lines = Vec::new();
    for (name, head, c) in [("PRIME", &s.chains.prime, p.alert), ("REGION", &s.chains.region, p.purple), ("ZONE", &s.chains.zone, p.fg)] {
        let (num, ago) = match head {
            Some(h) => (thousands(h.number), age(now - h.timestamp as i64)),
            None => ("—".into(), String::new()),
        };
        let pulse = head.as_ref().is_some_and(|h| now - (h.timestamp as i64) < 2);
        lines.push(Line::from(vec![
            Span::styled(if pulse { "◆ " } else { "◇ " }, Style::new().fg(c)),
            Span::styled(format!("{name:<7}"), Style::new().fg(c).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{num:>11}"), Style::new().fg(p.text)),
            Span::styled(format!("  {ago}"), Style::new().fg(p.dim)),
        ]));
    }
    lines.push(Line::raw(""));
    match &s.compare {
        Some(c) => {
            let rate = if c.compared > 0 { c.matched as f64 * 100.0 / c.compared as f64 } else { 0.0 };
            let ok = c.compared > 0 && c.matched == c.compared;
            let label = if app.theme == Theme::Angel { "シンクロ率" } else { "同期率" };
            lines.push(Line::from(vec![
                Span::styled(format!("{label} SYNC "), Style::new().fg(p.dim)),
                Span::styled(format!("{rate:5.1}%"), Style::new().fg(if ok { p.ok } else { p.alert }).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {}/{}", c.matched, c.compared), Style::new().fg(p.dim)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(format!("⇄ {} ", c.label), Style::new().fg(p.text)),
                Span::styled(
                    if c.online { thousands(c.height) } else { "offline".into() },
                    Style::new().fg(if c.online { p.text } else { p.alert }),
                ),
            ]));
            if let Some(m) = c.last_mismatch {
                lines.push(Line::styled(format!("DIFFERS at {}", thousands(m)), Style::new().fg(p.alert)));
            }
        }
        None => {
            if let Some(b) = s.blocks.back() {
                lines.push(kv(&p, "DIFFICULTY", group_digits(b.difficulty.parse().unwrap_or(0)), p.text));
                lines.push(kv(&p, "GAS", format!("{} / {}", thousands(b.gas_used), thousands(b.gas_limit)), p.text));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_peers(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let total = (s.peers.inbound + s.peers.outbound).max(1) as f64;
    let w = area.width.saturating_sub(16) as f64;
    let bar = |n: u32, c: Color| -> Vec<Span<'static>> {
        let k = ((n as f64 / total) * w).round() as usize;
        vec![
            Span::styled("█".repeat(k), Style::new().fg(c)),
            Span::styled("░".repeat((w as usize).saturating_sub(k)), Style::new().fg(p.faint)),
        ]
    };
    let mut l1 = vec![Span::styled(format!("IN  {:>4} ", s.peers.inbound), Style::new().fg(p.text))];
    l1.extend(bar(s.peers.inbound, p.fg));
    let mut l2 = vec![Span::styled(format!("OUT {:>4} ", s.peers.outbound), Style::new().fg(p.text))];
    l2.extend(bar(s.peers.outbound, p.purple));
    let l3 = Line::from(vec![
        Span::styled(format!("TOTAL {}  ", s.peers.count), Style::new().fg(p.fg).add_modifier(Modifier::BOLD)),
        Span::styled(format!("MAPPED {}  GEO {}", s.peers.list.iter().filter(|x| x.place.is_some()).count(), s.peers.geo), Style::new().fg(p.dim)),
    ]);
    f.render_widget(Paragraph::new(vec![Line::from(l1), Line::from(l2), l3]), area);
}

fn draw_econ(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    let mut lines = Vec::new();
    if let Some(b) = s.blocks.back() {
        lines.push(kv(&p, "BASE FEE", gwei(&b.base_fee), p.text));
        lines.push(kv(&p, "kQUAI", e18(&b.exchange_rate), p.text));
    }
    if let Some(m) = &s.mining {
        lines.push(kv(&p, "REWARD", format!("{} QUAI", m.estimated_block_reward), p.fg));
        lines.push(kv(&p, "WORKSHARE", format!("{} QUAI", m.workshare_reward), p.text));
        lines.push(kv(&p, "SUPPLY", format!("{} QUAI", group_digits(m.quai_supply.parse().unwrap_or(0))), p.text));
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
            let rows = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Length(3), Constraint::Min(0)]).split(inner);
            for (i, (name, _, a, pend)) in algo_rows(s).into_iter().enumerate() {
                let Some(&row) = rows.get(i) else { continue };
                let [label, spark] = Layout::horizontal([Constraint::Length(30), Constraint::Min(4)]).areas(row);
                let c = [p.fg, p.purple, p.warn][i];
                let text = vec![
                    Line::from(vec![
                        Span::styled(format!("{name:<7}"), Style::new().fg(c).add_modifier(Modifier::BOLD)),
                        Span::styled(si_rate(a.hashrate), Style::new().fg(p.text)),
                    ]),
                    Line::from(vec![
                        Span::styled(format!("share {:>5.2}s", a.share_time), Style::new().fg(p.dim)),
                        Span::styled(format!("  pending {pend}"), Style::new().fg(if pend > 0 { c } else { p.faint })),
                    ]),
                ];
                f.render_widget(Paragraph::new(text), label);
                spark_right(f, &app.hist[i], c, spark);
            }
        }
        Theme::Angel => {
            // MAGI-style triad: three verdict panels.
            let cols = Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Ratio(1, 3), Constraint::Ratio(1, 3)]).split(area);
            for (i, (name, n, a, pend)) in algo_rows(s).into_iter().enumerate() {
                let Some(&col) = cols.get(i) else { continue };
                let approved = a.hashrate > 0.0 && a.share_time > 0.0;
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Thick)
                    .border_style(Style::new().fg(if approved { p.fg } else { p.alert }))
                    .title(Line::from(Span::styled(
                        format!(" {name}·{n} "),
                        Style::new().fg(Color::Black).bg(if approved { p.fg } else { p.alert }).add_modifier(Modifier::BOLD),
                    )));
                let inner = block.inner(col);
                f.render_widget(block, col);
                let blink = approved || (app.tick / 5) % 2 == 0;
                let verdict = if approved { "承認 APPROVED" } else { "否決 DENIED" };
                let vstyle = if approved {
                    Style::new().fg(Color::Black).bg(p.ok).add_modifier(Modifier::BOLD)
                } else if blink {
                    Style::new().fg(Color::White).bg(p.alert).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(p.alert)
                };
                let text = vec![
                    Line::styled(si_rate(a.hashrate), Style::new().fg(p.warn).add_modifier(Modifier::BOLD)),
                    Line::styled(format!("SHARE {:.2}s", a.share_time), Style::new().fg(p.text)),
                    Line::styled(format!("PENDING {pend}"), Style::new().fg(p.text)),
                    Line::raw(""),
                    Line::styled(format!(" {verdict} "), vstyle),
                ];
                let [t, spark] = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(inner);
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
    let r = Rect { x: area.x + area.width - n as u16, width: n as u16, ..area };
    f.render_widget(Sparkline::default().data(&shifted).style(Style::new().fg(color)), r);
}

fn draw_map(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    if area.width < 10 || area.height < 4 {
        return;
    }
    let mut land = Vec::new();
    for y in 0..world::HEIGHT {
        for x in 0..world::WIDTH {
            if world::land(x, y) {
                land.push((-180.0 + (x as f64 + 0.5) * 1.5, 90.0 - (y as f64 + 0.5) * 1.5));
            }
        }
    }
    let peers: Vec<(f64, f64)> =
        s.peers.list.iter().filter_map(|x| x.place.as_ref()).map(|pl| (pl.lon, pl.lat)).collect();
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
            ctx.draw(&Points { coords: &land, color: land_c });
            ctx.layer();
            if let Some(h) = &here {
                for &(x, y) in &peers {
                    ctx.draw(&CLine { x1: h.lon, y1: h.lat, x2: x, y2: y, color: arc_c });
                }
            }
            ctx.layer();
            ctx.draw(&Points { coords: &peers, color: peer_c });
            if let Some(h) = &here {
                ctx.print(h.lon, h.lat, Span::styled("◎", Style::new().fg(alert)));
            }
        });
    f.render_widget(canvas, area);
    let mapped = s.peers.list.iter().filter(|x| x.place.is_some()).count();
    let note = if mapped == 0 {
        s.peers.note.clone().or_else(|| {
            (s.peers.geo == "off" && !s.peers.list.is_empty())
                .then(|| format!("{} peers; enable --geoip-db or --geoip-online to place them", s.peers.list.len()))
        })
    } else {
        None
    };
    let caption = note.unwrap_or_else(|| format!("{mapped} peers located · {} tcp", s.peers.list.len()));
    let cap_area = Rect { y: area.y + area.height - 1, height: 1, ..area };
    f.render_widget(Paragraph::new(Line::styled(caption, Style::new().fg(p.dim))).alignment(Alignment::Right), cap_area);
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
        buf.set_string(area.x, lane(k), name, Style::new().fg(tier[k]).add_modifier(Modifier::BOLD));
    }
    let slots = ((area.width - GUTTER) / 2) as usize;
    let blocks: Vec<_> = s.blocks.iter().rev().take(slots).collect::<Vec<_>>().into_iter().rev().collect();
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
                        c.set_symbol("─").set_fg(if k == 2 { p.dim } else { tier[k] });
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
            let color = if newest && fresh { p.warn } else { tier[k] };
            if let Some(c) = buf.cell_mut((x, lane(k))) {
                c.set_symbol(sym).set_fg(color);
                if newest && fresh {
                    c.set_style(Style::new().fg(color).add_modifier(Modifier::BOLD));
                }
            }
        }
    }
    // Latest prime and region numbers at the right end of their lanes.
    for (k, num) in [(0usize, blocks.iter().rev().find(|b| b.order == 0).map(|b| b.prime_number)), (1, blocks.iter().rev().find(|b| b.order <= 1).map(|b| b.region_number))] {
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
        lines.push(Line::styled("watching for prime and region blocks…", Style::new().fg(p.dim)));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_logs(f: &mut Frame, app: &App, s: &State, area: Rect) {
    let p = pal(app.theme);
    if s.logs.is_empty() {
        let msg = match &s.node.log_file {
            Some(file) => format!("following {file}"),
            None => "no log file: run with --logs <nodelogs dir or file>".into(),
        };
        f.render_widget(Paragraph::new(Line::styled(msg, Style::new().fg(p.dim))), area);
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
            let title = if fl.prime { "PRIME CONVERGENCE // 主鎖収束" } else { "REGION CONVERGENCE // 領域収束" };
            let w = (title.chars().count() as u16 + 10).max(fl.text.len() as u16 + 6).min(area.width);
            let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height / 2).saturating_sub(2), width: w, height: 5.min(area.height) };
            let on = left % 4 < 2;
            let (fg, bg) = if on { (p.bg, p.fg) } else { (p.fg, p.bg) };
            f.render_widget(Clear, r);
            let block = Block::default().borders(Borders::ALL).border_style(Style::new().fg(p.fg)).style(Style::new().bg(bg));
            let text = vec![
                Line::styled(title, Style::new().fg(fg).add_modifier(Modifier::BOLD)),
                Line::styled(fl.text.clone(), Style::new().fg(fg)),
            ];
            f.render_widget(Paragraph::new(text).block(block).alignment(Alignment::Center), r);
        }
        Theme::Angel => {
            let r = Rect { x: area.x, y: area.y + (area.height / 2).saturating_sub(3), width: area.width, height: 6.min(area.height) };
            let on = left % 4 < 2;
            let bg = if fl.prime { p.alert } else { p.purple };
            f.render_widget(Clear, r);
            let stripe: String = (0..area.width).map(|i| if ((i as u64 + app.tick) / 3) % 2 == 0 { '▞' } else { ' ' }).collect();
            let title = if fl.prime { "PATTERN PRIME — 主鎖確認" } else { "PATTERN REGION — 領域確認" };
            let text = vec![
                Line::styled(stripe.clone(), Style::new().fg(Color::Black).bg(bg)),
                Line::raw(""),
                Line::styled(title, Style::new().fg(if on { Color::White } else { Color::Black }).add_modifier(Modifier::BOLD)),
                Line::styled(fl.text.to_uppercase(), Style::new().fg(Color::Black)),
                Line::raw(""),
                Line::styled(stripe, Style::new().fg(Color::Black).bg(bg)),
            ];
            f.render_widget(Paragraph::new(text).style(Style::new().bg(bg)).alignment(Alignment::Center), r);
        }
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let p = pal(app.theme);
    let w = 46.min(area.width);
    let h = 11.min(area.height);
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, r);
    let inner = panel(f, r, app, "CONTROLS", "操作");
    let lines = vec![
        kv(&p, "t", "toggle GHOST / ANGEL".into(), p.text),
        kv(&p, "m", "full-screen peer map".into(), p.text),
        kv(&p, "l", "full-height node log".into(), p.text),
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
        let style = if last { Style::new().fg(p.bg).bg(p.fg).add_modifier(Modifier::BOLD) } else { Style::new().fg(p.fg) };
        lines.push(Line::styled(format!(" {st} "), style));
    }
    let w = 50.min(area.width) as usize;
    let done = (app.tick as usize * w) / BOOT_TICKS as usize;
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("█".repeat(done.min(w)), Style::new().fg(p.fg)),
        Span::styled("░".repeat(w.saturating_sub(done)), Style::new().fg(p.faint)),
    ]));
    lines.push(Line::styled(format!("{}  {}", s.node.label, s.node.rpc), Style::new().fg(p.dim)));
    let h = lines.len() as u16;
    let r = Rect { y: area.y + area.height.saturating_sub(h) / 2, height: h.min(area.height), ..area };
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
        s.node.location = "Cyprus-1".into();
        s.node.chain_id = Some(9);
        s.node.online = true;
        let t = now / 1000;
        s.chains.prime = Some(Head { number: 2_297_888, hash: "0xaa".into(), timestamp: t - 40 });
        s.chains.region = Some(Head { number: 5_579_748, hash: "0xbb".into(), timestamp: t - 12 });
        s.chains.zone = Some(Head { number: 10_390_405, hash: "0x435ba623ce10a380".into(), timestamp: t - 3 });
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
                order: if i == 40 { 0 } else if i % 9 == 0 { 1 } else { 2 },
                difficulty: "1038556905100".into(),
                exchange_rate: "13264669140000000000".into(),
                ..Default::default()
            });
        }
        s.mining = Some(Mining {
            avg_block_time: 5.257,
            blocks_analyzed: 172,
            kawpow: Algo { hashrate: 243_688_736_202.0, difficulty: "1".into(), share_time: 5.2 },
            sha: Algo { hashrate: 2.56e17, difficulty: "1".into(), share_time: 1.27 },
            scrypt: Algo { hashrate: 0.0, difficulty: "1".into(), share_time: 0.0 },
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
        s.peers.here = Some(Place { lat: 41.9, lon: -87.6, city: String::new(), country: "US".into() });
        for (lat, lon) in [(52.5, 13.4), (35.7, 139.7), (-33.9, 151.2), (40.7, -74.0), (1.35, 103.8)] {
            s.peers.list.push(Peer {
                ip: "203.0.113.7".into(),
                port: 4002,
                dir: "in".into(),
                place: Some(Place { lat, lon, city: String::new(), country: String::new() }),
                since_ms: now,
            });
        }
        s.compare = Some(Compare { label: "GO-QUAI".into(), rpc: "x".into(), height: 10_390_405, online: true, compared: 64, matched: 64, last_mismatch: None });
        s.push_event(now - 5000, "prime", "Prime block 2297888 (zone 10390386)".into(), Some(10_390_386));
        s.push_event(now - 2000, "region", "Region block 5579748 (zone 10390400)".into(), Some(10_390_400));
        s.push_log("INFO".into(), "2026-10-01T17:23:03Z  INFO rsq_node::node: network peers=85".into());
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
        app.hist = [VecDeque::from(vec![1100, 1110, 1105]), VecDeque::from(vec![1740, 1741]), VecDeque::new()];
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
                x += if sym.chars().next().is_some_and(wide) { 2 } else { 1 };
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
        for want in ["QUAI//DIVE", "RS-QUAI SOAK", "Cyprus-1", "HIERARCHY", "MERGED MINING", "243.69 GH/s", "256.00 PH/s", "PEER MAP", "NODE LOG", "network peers=85", "100.0%", "BLOCK LATTICE"] {
            assert!(g.contains(want), "GHOST frame lacks {want:?}\n{g}");
        }
        let a = render(Theme::Angel, 160, 48, View::Dash);
        for want in ["QUAI TERMINAL", "KAWPOW·1", "SHA·2", "SCRYPT·3", "承認 APPROVED", "否決 DENIED", "ECONOMY", "105.3384 QUAI", "27,231 Gwei", "13.2646"] {
            assert!(a.contains(want), "ANGEL frame lacks {want:?}\n{a}");
        }
        let small = render(Theme::Angel, 90, 26, View::Dash);
        assert!(small.contains("ZONE HEIGHT") && small.contains("NODE LOG"), "{small}");
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
                    app.flash = Some(Flash { until: tick + 5, prime: tick % 2 == 0, text: "Prime block 1 (zone 2)".into() });
                    let mut offline = s.clone();
                    offline.node.online = tick == 0;
                    offline.node.error = Some("connection refused".into());
                    assert!(term.draw(|f| draw(f, &app, &offline)).is_ok());
                }
            }
        }
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
