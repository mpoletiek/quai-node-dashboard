//! Follows the node's log file (`tail -F`). Both nodes write `nodelogs/`
//! in their working directory, in different layouts and formats:
//!
//! - rs-quai: `global.log` only, `tracing` lines:
//!   `2026-10-01T17:23:03.275792Z  INFO rsq_node::node: network peers=72`.
//! - go-quai: `global.log` plus one log per chain (`prime.log`,
//!   `region-N.log`, `zone-N-M.log`), logrus text with colours, the level
//!   padded to 7 and a `01-02|15:04:05.000` timestamp (`log/logger.go`):
//!   `INFO   [10-01|09:03:52.206] Appended new block   number=…`.
//!
//! Survives rotation (the file shrinks or is replaced) and strips ANSI
//! colours.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::detect::NodeKind;
use crate::state::State;

/// Picks the log to follow from a file or a `nodelogs` directory: for
/// go-quai the node's zone log (`zone-R-Z.log` for `location`, else the
/// first zone log), for rs-quai `global.log`.
pub fn resolve(path: &Path, kind: NodeKind, location: Option<(u64, u64)>) -> PathBuf {
    if !path.is_dir() {
        return path.to_path_buf();
    }
    let mut names: Vec<String> = Vec::new();
    match kind {
        NodeKind::RsQuai => names.push("global.log".into()),
        NodeKind::GoQuai | NodeKind::Unknown => {
            if let Some((r, z)) = location {
                names.push(format!("zone-{r}-{z}.log"));
            }
            names.push("zone-0-0.log".into());
            let mut zones: Vec<String> = std::fs::read_dir(path)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.starts_with("zone-") && n.ends_with(".log"))
                .collect();
            zones.sort();
            names.extend(zones);
            names.push("global.log".into());
        }
    }
    names
        .iter()
        .map(|n| path.join(n))
        .find(|p| p.is_file())
        .unwrap_or_else(|| path.join(names.last().map_or("global.log", String::as_str)))
}

/// A `tracing` line (rs-quai): RFC 3339 UTC timestamp, the level, then
/// `target:`. Returns the level.
pub fn parse_tracing(line: &str) -> Option<&'static str> {
    let mut words = line.split_whitespace();
    let ts = words.next()?;
    let b = ts.as_bytes();
    let stamp = b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && ts.ends_with('Z');
    if !stamp {
        return None;
    }
    let level = norm_level(words.next()?)?;
    words.next()?.ends_with(':').then_some(level)
}

/// A logrus text line (go-quai): the level (padded), then
/// `[MM-DD|HH:MM:SS.mmm]`. Returns the level.
pub fn parse_logrus(line: &str) -> Option<&'static str> {
    let (word, rest) = line.split_once('[')?;
    let level = norm_level(word.trim_end())?;
    let stamp = rest.get(..18)?.as_bytes();
    let ok = stamp[2] == b'-'
        && stamp[5] == b'|'
        && stamp[8] == b':'
        && stamp[11] == b':'
        && stamp[14] == b'.'
        && rest.as_bytes().get(18) == Some(&b']');
    ok.then_some(level)
}

fn norm_level(word: &str) -> Option<&'static str> {
    Some(match word {
        "ERROR" | "FATAL" | "PANIC" => "ERROR",
        "WARN" | "WARNING" => "WARN",
        "INFO" => "INFO",
        "DEBUG" => "DEBUG",
        "TRACE" => "TRACE",
        _ => return None,
    })
}

/// Which node wrote a (colour-stripped) log line, by its format.
pub fn format_of(line: &str) -> NodeKind {
    if parse_tracing(line).is_some() {
        NodeKind::RsQuai
    } else if parse_logrus(line).is_some() {
        NodeKind::GoQuai
    } else {
        NodeKind::Unknown
    }
}

/// A line's level with the node's own format, falling back to the other
/// format and then to [`level_of`].
pub fn level_for(kind: NodeKind, line: &str) -> String {
    let parsed = match kind {
        NodeKind::GoQuai => parse_logrus(line).or_else(|| parse_tracing(line)),
        NodeKind::RsQuai | NodeKind::Unknown => parse_tracing(line).or_else(|| parse_logrus(line)),
    };
    parsed.map_or_else(|| level_of(line), str::to_string)
}

/// Removes ANSI escape sequences.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The level word near the start of a line (`WARNING` reads as `WARN`).
pub fn level_of(line: &str) -> String {
    let head: String = line.chars().take(48).collect();
    for (word, level) in [
        ("ERROR", "ERROR"),
        ("FATAL", "ERROR"),
        ("PANIC", "ERROR"),
        ("WARN", "WARN"),
        ("INFO", "INFO"),
        ("DEBUG", "DEBUG"),
        ("TRACE", "TRACE"),
    ] {
        if head
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|w| w == word || (word == "WARN" && w == "WARNING"))
        {
            return level.to_string();
        }
    }
    String::new()
}

/// Longest log line kept; the rest of a longer one is dropped.
pub const MAX_LINE: usize = 4096;
/// Most read in one poll.
const READ_CHUNK: u64 = 1 << 20;
/// Further behind than this, the follower skips ahead to the last 16 KiB
/// (only the last few hundred lines are shown anyway).
const MAX_BEHIND: u64 = 4 << 20;
/// Where reading starts in a file: its last 16 KiB.
const TAIL: u64 = 16 << 10;

/// Splits a byte stream into lines of at most `MAX_LINE` bytes.
#[derive(Default)]
struct Lines {
    /// The line so far.
    partial: Vec<u8>,
    /// Dropping the rest of the current line: it was cut (too long), or
    /// it is the first after a seek into the middle of the file.
    skip: bool,
}

impl Lines {
    /// Starts over; `mid_line` drops everything up to the next newline.
    fn reset(&mut self, mid_line: bool) {
        self.partial.clear();
        self.skip = mid_line;
    }

    /// Feeds `buf`; returns the lines it completes (and a too-long line,
    /// cut, as soon as it is too long).
    fn feed(&mut self, buf: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        let mut segs = buf.split(|&b| b == b'\n').peekable();
        while let Some(seg) = segs.next() {
            let ends = segs.peek().is_some();
            if !self.skip {
                let room = MAX_LINE.saturating_sub(self.partial.len());
                self.partial.extend_from_slice(&seg[..seg.len().min(room)]);
                if seg.len() > room {
                    let mut l = String::from_utf8_lossy(&self.partial).into_owned();
                    l.push('…');
                    out.push(l);
                    self.partial.clear();
                    self.skip = !ends;
                } else if ends {
                    out.push(String::from_utf8_lossy(&self.partial).into_owned());
                    self.partial.clear();
                }
            } else if ends {
                self.skip = false;
            }
        }
        out
    }
}

/// `(device, inode)` of a file.
fn identity(m: &std::fs::Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}

/// Follows `path` forever, appending complete lines to the state. With
/// `owner`, only a file that user owns is read (checked at every open,
/// after symlinks): a detected log can't be swapped for someone else's
/// file.
pub fn follow(path: PathBuf, kind: NodeKind, owner: Option<u32>, state: Arc<Mutex<State>>) {
    let mut file: Option<(File, (u64, u64))> = None;
    let mut pos = 0u64;
    let mut lines = Lines::default();
    loop {
        if file.is_none() {
            let opened = File::open(&path).ok().and_then(|f| {
                let m = f.metadata().ok()?;
                (owner.is_none() || Some(m.uid()) == owner).then_some((f, m))
            });
            if let Some((mut f, m)) = opened {
                pos = m.len().saturating_sub(TAIL);
                if f.seek(SeekFrom::Start(pos)).is_ok() {
                    lines.reset(pos > 0);
                    file = Some((f, identity(&m)));
                }
            }
        }
        let mut reopen = false;
        let mut new = Vec::new();
        if let Some((f, id)) = file.as_mut() {
            // Rotated: the path now names a shorter or different file.
            match std::fs::metadata(&path) {
                Ok(m) if identity(&m) == *id && m.len() >= pos => {
                    if m.len() - pos > MAX_BEHIND && f.seek(SeekFrom::Start(m.len() - TAIL)).is_ok()
                    {
                        pos = m.len() - TAIL;
                        lines.reset(true);
                    }
                    let mut buf = Vec::new();
                    if f.take(READ_CHUNK).read_to_end(&mut buf).is_ok() {
                        pos += buf.len() as u64;
                        new = lines.feed(&buf);
                    }
                }
                _ => reopen = true,
            }
        }
        if !new.is_empty() {
            if let Ok(mut st) = state.lock() {
                for l in new {
                    let text = strip_ansi(l.trim_end());
                    if text.is_empty() {
                        continue;
                    }
                    let level = level_for(kind, &text);
                    st.push_log(level, text);
                }
            }
        }
        if reopen {
            file = None;
            continue;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_colours() {
        let go = "\u{1b}[33mWARNING\u{1b}[0m[10-01|09:03:52.206] Devnet overrides active";
        assert_eq!(
            strip_ansi(go),
            "WARNING[10-01|09:03:52.206] Devnet overrides active"
        );
        assert_eq!(level_of(&strip_ansi(go)), "WARN");
        assert_eq!(
            level_of("2026-10-01T17:23:03.275792Z  INFO rsq_node::node: network peers=72"),
            "INFO"
        );
        assert_eq!(
            level_of("2026-10-01T17:23:03Z ERROR rsq_chain: Append failed"),
            "ERROR"
        );
        assert_eq!(level_of("plain text"), "");
    }

    #[test]
    fn rs_quai_lines() {
        let l = "2026-10-01T17:23:03.275792Z  INFO rsq_node::node: network peers=72";
        assert_eq!(parse_tracing(l), Some("INFO"));
        assert_eq!(format_of(l), NodeKind::RsQuai);
        let w = "2026-10-08T07:38:36.911961Z  WARN rsq_stratum::server: ERROR from miner";
        assert_eq!(parse_tracing(w), Some("WARN"));
        // The heuristic would see the ERROR in the message; the parser
        // reads the level field.
        assert_eq!(level_for(NodeKind::RsQuai, w), "WARN");
        assert_eq!(
            parse_tracing("2026-10-01T17:23:03Z ERROR rsq_chain: Append failed"),
            Some("ERROR")
        );
        assert_eq!(parse_tracing("INFO   [10-01|09:03:52.206] Started"), None);
        assert_eq!(parse_tracing("2026-10-01 17:23:03 INFO x: y"), None);
    }

    #[test]
    fn go_quai_lines() {
        // logrus TextFormatter{ForceColors, PadLevelText, FullTimestamp,
        // TimestampFormat: "01-02|15:04:05.000"}, as go-quai writes it.
        let raw = "\u{1b}[36mINFO   \u{1b}[0m[10-01|09:03:52.206] Appended new block                           \u{1b}[36mnumber\u{1b}[0m=10504202 \u{1b}[36mhash\u{1b}[0m=0x00ab";
        let l = strip_ansi(raw);
        assert!(l.starts_with("INFO   [10-01|09:03:52.206] Appended"), "{l}");
        assert_eq!(parse_logrus(&l), Some("INFO"));
        assert_eq!(format_of(&l), NodeKind::GoQuai);
        assert_eq!(
            parse_logrus("WARNING[10-01|09:03:52.206] Devnet overrides active"),
            Some("WARN")
        );
        assert_eq!(
            parse_logrus("FATAL  [12-31|23:59:59.999] Database corrupt"),
            Some("ERROR")
        );
        assert_eq!(
            level_for(NodeKind::GoQuai, "DEBUG  [10-01|09:03:52.206] INFO request"),
            "DEBUG"
        );
        assert_eq!(parse_logrus("INFO [not a stamp] x"), None);
        assert_eq!(format_of("plain text"), NodeKind::Unknown);
    }

    #[test]
    fn lines_are_bounded() {
        let mut l = Lines::default();
        assert_eq!(l.feed(b"one\ntw"), vec!["one"]);
        assert_eq!(l.feed(b"o\nthree"), vec!["two"]);
        // A line far over the limit, in pieces: cut once, the rest dropped.
        let long = vec![b'x'; MAX_LINE * 3];
        let out = l.feed(&long);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chars().count(), MAX_LINE + 1);
        assert!(out[0].starts_with("threexx") && out[0].ends_with('…'));
        assert!(l.feed(&long).is_empty());
        assert!(l.partial.is_empty());
        assert_eq!(l.feed(b"tail\nnext\n"), vec!["next"]);
        // Starting mid-file drops the cut first line.
        l.reset(true);
        assert_eq!(l.feed(b"ut line\nwhole\n"), vec!["whole"]);
        // Invalid UTF-8 is replaced, not trusted.
        assert_eq!(l.feed(b"\xff\xfe ok\n"), vec!["\u{fffd}\u{fffd} ok"]);
    }

    #[test]
    fn rotation_by_replacement_is_followed() {
        let dir = std::env::temp_dir().join(format!("quai-dash-rot-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("global.log");
        let _ = std::fs::write(&path, "2026-10-01T17:23:03Z  INFO a: old\n");
        let st = Arc::new(Mutex::new(State::default()));
        let (p, s) = (path.clone(), st.clone());
        std::thread::spawn(move || follow(p, NodeKind::RsQuai, None, s));
        std::thread::sleep(Duration::from_millis(300));
        // Replaced by a longer file: the old size check missed this.
        let _ = std::fs::write(
            dir.join("new"),
            "2026-10-01T17:23:04Z  INFO a: new, and longer than the old one\n",
        );
        let _ = std::fs::rename(dir.join("new"), &path);
        std::thread::sleep(Duration::from_millis(1000));
        let logs: Vec<String> = st
            .lock()
            .map(|s| s.logs.iter().map(|l| l.text.clone()).collect())
            .unwrap_or_default();
        assert!(
            logs.last().is_some_and(|l| l.contains("new, and longer")),
            "{logs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn follows_only_the_owners_file() {
        let path = std::env::temp_dir().join(format!("quai-dash-owner-{}.log", std::process::id()));
        let _ = std::fs::write(&path, "2026-10-01T17:23:03Z  INFO a: one\n");
        let me = std::fs::metadata(&path).map(|m| m.uid()).ok();
        let run = |owner: Option<u32>| {
            let st = Arc::new(Mutex::new(State::default()));
            let (p, s) = (path.clone(), st.clone());
            std::thread::spawn(move || follow(p, NodeKind::RsQuai, owner, s));
            std::thread::sleep(Duration::from_millis(300));
            st.lock().map(|s| s.logs.len()).unwrap_or(0)
        };
        assert_eq!(run(me), 1);
        assert_eq!(run(None), 1);
        assert_eq!(run(me.map(|u| u.wrapping_add(1))), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn log_file_choice() {
        let dir = std::env::temp_dir().join(format!("quai-dash-logs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        for n in [
            "global.log",
            "prime.log",
            "region-0.log",
            "zone-0-0.log",
            "zone-1-2.log",
        ] {
            let _ = std::fs::write(dir.join(n), b"");
        }
        assert_eq!(
            resolve(&dir, NodeKind::RsQuai, None),
            dir.join("global.log")
        );
        assert_eq!(
            resolve(&dir, NodeKind::GoQuai, None),
            dir.join("zone-0-0.log")
        );
        assert_eq!(
            resolve(&dir, NodeKind::GoQuai, Some((1, 2))),
            dir.join("zone-1-2.log")
        );
        let _ = std::fs::remove_file(dir.join("zone-0-0.log"));
        assert_eq!(
            resolve(&dir, NodeKind::Unknown, None),
            dir.join("zone-1-2.log")
        );
        let file = dir.join("prime.log");
        assert_eq!(resolve(&file, NodeKind::RsQuai, None), file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
