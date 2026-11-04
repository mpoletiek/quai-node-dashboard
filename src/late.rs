//! Detection again, for what the first look missed. A node that starts
//! after quai-dash (both at boot, say) has no process, logs or stratum to
//! find yet, and nothing would look again. While something detectable is
//! still missing, detection reruns every [`EVERY`]; what turns up is
//! handed over: the node kind to the state, a log file to a new follower,
//! a stratum API to the collector. Only gaps are filled: nothing found
//! before, or set by the user, is changed.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::detect::{self, Detected, Given, NodeKind};
use crate::logs;
use crate::rpc::Endpoint;
use crate::state::{State, now_ms};
use crate::term;

/// How often to look again.
pub const EVERY: Duration = Duration::from_secs(15);

/// What the first detection left open that a later one could fill: each
/// is neither set by the user nor detected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gaps {
    /// The node kind is unknown.
    pub kind: bool,
    /// No log file.
    pub logs: bool,
    /// No stratum API.
    pub stratum: bool,
}

impl Gaps {
    /// Anything left to find.
    pub fn any(self) -> bool {
        self.kind || self.logs || self.stratum
    }

    /// The missing ones, for a message (`node kind, logs, stratum API`).
    pub fn names(self) -> String {
        [
            (self.kind, "node kind"),
            (self.logs, "logs"),
            (self.stratum, "stratum API"),
        ]
        .iter()
        .filter(|(g, _)| *g)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// What a later detection found for the gaps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// The node kind, and why.
    pub kind: Option<(NodeKind, String)>,
    /// The `nodelogs` directory and the user that must own its files.
    pub logs: Option<(PathBuf, Option<u32>)>,
    /// The stratum API's URL.
    pub stratum: Option<String>,
}

/// The part of `d` that fills `gaps`.
pub fn fill(gaps: Gaps, d: &Detected) -> Found {
    Found {
        kind: (gaps.kind && d.kind != NodeKind::Unknown).then(|| (d.kind, d.kind_why.clone())),
        logs: d
            .logs
            .clone()
            .filter(|_| gaps.logs)
            .map(|l| (l, d.process.as_ref().and_then(|p| p.uid))),
        stratum: d.stratum.clone().filter(|_| gaps.stratum),
    }
}

/// What to look for and where the findings go.
pub struct Watch {
    /// Zone RPC.
    pub zone: Endpoint,
    /// What is missing.
    pub gaps: Gaps,
    /// The kind known so far (`Unknown` when it is a gap).
    pub kind: NodeKind,
    /// The node PID the user gave, if any.
    pub pid: Option<u32>,
    /// The collector, for a stratum API.
    pub stratum: Sender<Endpoint>,
}

/// Looks again every [`EVERY`] until every gap is filled.
pub fn run(mut w: Watch, state: Arc<Mutex<State>>) {
    // Detection quotes other processes (names, paths): printable only.
    let say = |line: String| eprintln!("{}", term::printable(&line, false));
    while w.gaps.any() {
        std::thread::sleep(EVERY);
        let d = detect::detect(&Given {
            zone: &w.zone,
            kind: (!w.gaps.kind).then_some(w.kind),
            pid: w.pid,
            logs: None,
            stratum: !w.gaps.stratum,
        });
        let found = fill(w.gaps, &d);
        let mut news = Vec::new();
        if let Some((k, why)) = found.kind {
            w.kind = k;
            w.gaps.kind = false;
            if let Ok(mut st) = state.lock() {
                st.node.kind = k.name().to_string();
                st.node.kind_how = why.clone();
            }
            news.push(format!("{} node ({why})", k.name()));
        }
        if let Some((dir, owner)) = found.logs {
            w.gaps.logs = false;
            let file = logs::resolve(&dir, w.kind, d.location);
            if let Ok(mut st) = state.lock() {
                st.node.log_file = Some(file.display().to_string());
            }
            news.push(format!("logs {}", file.display()));
            let (kind, s) = (w.kind, state.clone());
            std::thread::spawn(move || logs::follow(file, kind, owner, s));
        }
        if let Some(url) = found.stratum {
            if let Ok(ep) = Endpoint::new(&url, Duration::from_secs(4)) {
                w.gaps.stratum = false;
                // The collector polls it from its next tick.
                let _ = w.stratum.send(ep);
                news.push(format!("stratum {url}"));
            }
        }
        if !news.is_empty() {
            let text = format!("Found later: {}", news.join(" · "));
            say(format!("quai-dash: {text}"));
            if let Ok(mut st) = state.lock() {
                st.push_event(now_ms(), "found", text, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Process;

    fn detected() -> Detected {
        Detected {
            kind: NodeKind::RsQuai,
            kind_why: "process rs-quai (pid 7)".into(),
            process: Some(Process {
                pid: 7,
                uid: Some(1002),
                ..Default::default()
            }),
            logs: Some(PathBuf::from("/srv/node/nodelogs")),
            stratum: Some("http://127.0.0.1:3336".into()),
            ..Default::default()
        }
    }

    #[test]
    fn fills_only_the_gaps() {
        let all = Gaps {
            kind: true,
            logs: true,
            stratum: true,
        };
        let f = fill(all, &detected());
        assert_eq!(
            f.kind,
            Some((NodeKind::RsQuai, "process rs-quai (pid 7)".into()))
        );
        assert_eq!(
            f.logs,
            Some((PathBuf::from("/srv/node/nodelogs"), Some(1002)))
        );
        assert_eq!(f.stratum.as_deref(), Some("http://127.0.0.1:3336"));
        // What was there already stays as it was.
        let only_stratum = Gaps {
            stratum: true,
            ..Default::default()
        };
        assert_eq!(
            fill(only_stratum, &detected()),
            Found {
                stratum: Some("http://127.0.0.1:3336".into()),
                ..Default::default()
            }
        );
        assert_eq!(fill(Gaps::default(), &detected()), Found::default());
    }

    #[test]
    fn nothing_found_fills_nothing() {
        let all = Gaps {
            kind: true,
            logs: true,
            stratum: true,
        };
        assert_eq!(fill(all, &Detected::default()), Found::default());
    }

    #[test]
    fn names_the_gaps() {
        assert!(!Gaps::default().any());
        let g = Gaps {
            kind: true,
            logs: false,
            stratum: true,
        };
        assert!(g.any());
        assert_eq!(g.names(), "node kind, stratum API");
    }
}
