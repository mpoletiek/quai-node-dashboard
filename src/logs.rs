//! Follows the node's log file (`tail -F`): both nodes write `nodelogs/`;
//! go-quai splits per chain (`zone-0-0.log`), rs-quai writes `global.log`.
//! Survives rotation (the file shrinks or is replaced) and strips ANSI
//! colours.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::State;

/// Picks the log to follow from a file or a `nodelogs` directory.
pub fn resolve(path: &Path) -> PathBuf {
    if path.is_dir() {
        for name in ["zone-0-0.log", "global.log"] {
            let p = path.join(name);
            if p.exists() {
                return p;
            }
        }
    }
    path.to_path_buf()
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

/// Follows `path` forever, appending complete lines to the state.
pub fn follow(path: PathBuf, state: Arc<Mutex<State>>) {
    let mut file: Option<File> = None;
    let mut pos = 0u64;
    let mut partial = String::new();
    loop {
        if file.is_none() {
            if let Ok(mut f) = File::open(&path) {
                // Start near the end: the last 16 KiB.
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                pos = len.saturating_sub(16 * 1024);
                if f.seek(SeekFrom::Start(pos)).is_ok() {
                    partial.clear();
                    if pos > 0 {
                        partial.push('\u{0}'); // drop the first, cut line
                    }
                    file = Some(f);
                }
            }
        }
        let mut reopen = false;
        if let Some(f) = file.as_mut() {
            // Rotated: the path now names a shorter or different file.
            let disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if disk < pos {
                reopen = true;
            } else {
                let mut buf = Vec::new();
                if f.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                    pos += buf.len() as u64;
                    partial.push_str(&String::from_utf8_lossy(&buf));
                    let mut lines: Vec<String> = partial.split('\n').map(str::to_string).collect();
                    partial = lines.pop().unwrap_or_default();
                    if let Ok(mut st) = state.lock() {
                        for l in lines {
                            if l.starts_with('\u{0}') {
                                continue;
                            }
                            let text = strip_ansi(l.trim_end());
                            if text.is_empty() {
                                continue;
                            }
                            let level = level_of(&text);
                            st.push_log(level, text);
                        }
                    }
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
}
