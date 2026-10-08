//! Finds the node on this host and works out what it is, so `quai-dash
//! web` needs no flags next to a node.
//!
//! - **Process:** whoever listens on the zone RPC port (from
//!   `/proc/net/tcp` and each process's `fd` links), else a process named
//!   `rs-quai` or `go-quai`. Another user's process can be named but not
//!   inspected (its `cwd`, `exe` and sockets need that user or root).
//! - **Logs:** both nodes write `nodelogs/` in their working directory;
//!   failing that, under `--global.data-dir`.
//! - **Kind**, strongest evidence first: the process name; the log layout
//!   (go-quai writes per-chain logs, and the two write different line
//!   formats); the stratum API (rs-quai's `/api/pool/stats` has `mined`);
//!   and last, the RPC's header casing (go-quai `Content-Type`, rs-quai
//!   `content-type`). The two are RPC-identical otherwise.
//! - **Stratum API:** the node's `--node.stratum-api-addr`, else port 3336
//!   on the RPC host, if it answers.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::logs;
use crate::peers::{self, Listener};
use crate::rpc::{Endpoint, hex_u64};

/// Which node implementation is being watched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum NodeKind {
    /// The Rust port.
    RsQuai,
    /// The Go reference.
    GoQuai,
    /// Not known.
    #[default]
    Unknown,
}

impl NodeKind {
    /// `rs-quai`, `go-quai` or `unknown`.
    pub fn name(self) -> &'static str {
        match self {
            NodeKind::RsQuai => "rs-quai",
            NodeKind::GoQuai => "go-quai",
            NodeKind::Unknown => "unknown",
        }
    }
}

/// `--node-kind`: a kind, or detect it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KindChoice {
    /// Detect it.
    Auto,
    /// rs-quai.
    RsQuai,
    /// go-quai.
    GoQuai,
}

impl KindChoice {
    /// The kind it forces, if any.
    pub fn forced(self) -> Option<NodeKind> {
        match self {
            KindChoice::Auto => None,
            KindChoice::RsQuai => Some(NodeKind::RsQuai),
            KindChoice::GoQuai => Some(NodeKind::GoQuai),
        }
    }
}

/// The kind a program name (`rs-quai`, `/opt/bin/go-quai`, …) suggests.
pub fn kind_from_name(name: &str) -> NodeKind {
    let base = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    if base.starts_with("rs-quai") || base.starts_with("rsq-node") {
        NodeKind::RsQuai
    } else if base.starts_with("go-quai") {
        NodeKind::GoQuai
    } else {
        NodeKind::Unknown
    }
}

/// The kind a `nodelogs` directory's layout and contents suggest, and why.
pub fn kind_from_logs(dir: &Path) -> Option<(NodeKind, String)> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    kind_from_log_names(&names).or_else(|| {
        let tail = read_tail(&dir.join("global.log"), 8 * 1024)?;
        kind_from_log_text(&tail).map(|k| (k, "global.log line format".to_string()))
    })
}

/// go-quai's per-chain log files (`prime.log`, `region-N.log`,
/// `zone-N-M.log`); rs-quai writes `global.log` only.
pub fn kind_from_log_names(names: &[String]) -> Option<(NodeKind, String)> {
    names
        .iter()
        .find(|n| {
            n.as_str() == "prime.log"
                || (n.ends_with(".log") && (n.starts_with("region-") || n.starts_with("zone-")))
        })
        .map(|n| (NodeKind::GoQuai, format!("per-chain log {n}")))
}

/// The format of the first recognisable line of `text`.
pub fn kind_from_log_text(text: &str) -> Option<NodeKind> {
    text.lines()
        .map(|l| logs::format_of(&logs::strip_ansi(l)))
        .find(|k| *k != NodeKind::Unknown)
}

fn read_tail(path: &Path, max: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The kind a stratum `/api/pool/stats` answer suggests: rs-quai's has a
/// `mined` tally, go-quai's does not.
pub fn kind_from_stratum(stats: &Value) -> NodeKind {
    if stats.get("mined").is_some_and(Value::is_object) {
        NodeKind::RsQuai
    } else {
        NodeKind::GoQuai
    }
}

/// The kind an RPC reply's `Content-Type` spelling suggests (a weak hint:
/// Go's net/http canonicalises header names, rs-quai's server lowercases).
pub fn kind_from_header_names(headers: &[(String, String)]) -> NodeKind {
    match headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        Some((k, _)) if k == "Content-Type" => NodeKind::GoQuai,
        Some((k, _)) if k == "content-type" => NodeKind::RsQuai,
        _ => NodeKind::Unknown,
    }
}

/// The value of `--long` / `-s` in an argument list (`--x v`, `--x=v`).
pub fn flag_value(args: &[String], long: &str, short: Option<&str>) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        for name in std::iter::once(long).chain(short) {
            if a == name {
                return it.next().cloned();
            }
            if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// A process seen through `/proc`.
#[derive(Clone, Debug, Default)]
pub struct Process {
    /// PID.
    pub pid: u32,
    /// Program name (executable, else `argv[0]`, else `comm`).
    pub name: String,
    /// Command line.
    pub args: Vec<String>,
    /// Working directory, when readable (same user or root).
    pub cwd: Option<PathBuf>,
    /// How it was found.
    pub how: String,
}

impl Process {
    /// The kind its name suggests.
    pub fn kind(&self) -> NodeKind {
        kind_from_name(&self.name)
    }

    /// Where its `nodelogs` should be: the working directory's, else the
    /// data directory's.
    pub fn logs_dir(&self) -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.cwd.iter().map(|c| c.join("nodelogs")).collect();
        if let Some(d) = flag_value(&self.args, "--global.data-dir", Some("-d")) {
            let d = PathBuf::from(d);
            let d = match (&self.cwd, d.is_relative()) {
                (Some(c), true) => c.join(d),
                _ => d,
            };
            dirs.push(d.join("nodelogs"));
        }
        dirs.into_iter().find(|d| d.is_dir())
    }

    /// The stratum API address on its command line, as a URL.
    pub fn stratum_api(&self) -> Option<String> {
        let a = flag_value(&self.args, "--node.stratum-api-addr", None)?;
        let a = a.trim_start_matches("http://");
        let a = match a.rsplit_once(':') {
            Some(("" | "0.0.0.0" | "[::]" | "::", port)) => format!("127.0.0.1:{port}"),
            _ => a.to_string(),
        };
        Some(format!("http://{a}"))
    }
}

/// Reads `/proc/<pid>`; `None` when it doesn't exist.
pub fn read_process(pid: u32, how: &str) -> Option<Process> {
    let dir = PathBuf::from(format!("/proc/{pid}"));
    let comm = std::fs::read_to_string(dir.join("comm")).ok()?;
    let args: Vec<String> = std::fs::read(dir.join("cmdline"))
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .unwrap_or_default();
    let exe = std::fs::read_link(dir.join("exe")).ok();
    let name = exe
        .as_ref()
        .and_then(|e| e.file_name())
        .map(|n| {
            n.to_string_lossy()
                .trim_end_matches(" (deleted)")
                .to_string()
        })
        .or_else(|| {
            args.first()
                .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
        })
        .unwrap_or_else(|| comm.trim().to_string());
    Some(Process {
        pid,
        name,
        args,
        cwd: std::fs::read_link(dir.join("cwd")).ok(),
        how: how.to_string(),
    })
}

/// Processes whose `comm` names a node (readable for every user).
fn by_name() -> Vec<u32> {
    let mut out: Vec<u32> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse().ok()))
        .filter(|pid: &u32| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|c| kind_from_name(c.trim()) != NodeKind::Unknown)
        })
        .collect();
    out.sort_unstable();
    out
}

/// What the user already decided; detection fills in the rest.
pub struct Given<'a> {
    /// Zone RPC.
    pub zone: &'a Endpoint,
    /// Forced kind, if any.
    pub kind: Option<NodeKind>,
    /// Node PID, if given.
    pub pid: Option<u32>,
    /// Logs, if given.
    pub logs: Option<&'a Path>,
    /// Whether the stratum API was given (or turned off).
    pub stratum: bool,
}

/// What detection found.
#[derive(Clone, Debug, Default)]
pub struct Detected {
    /// The node kind (`Unknown` when nothing told).
    pub kind: NodeKind,
    /// Why that kind.
    pub kind_why: String,
    /// The node process.
    pub process: Option<Process>,
    /// Its `nodelogs` directory.
    pub logs: Option<PathBuf>,
    /// Its stratum API, if one answered.
    pub stratum: Option<String>,
    /// `quai_nodeLocation` as `(region, zone)`.
    pub location: Option<(u64, u64)>,
    /// Things worth telling the user (unreadable process, …).
    pub notes: Vec<String>,
}

fn is_local(host: &str) -> bool {
    matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0.0.0.0"
    )
}

/// Looks for the node and works out its kind, logs and stratum.
pub fn detect(g: &Given) -> Detected {
    let mut d = Detected::default();
    let local = is_local(g.zone.host());
    // The process.
    if let Some(pid) = g.pid {
        d.process = read_process(pid, "--node-pid");
        if d.process.is_none() {
            d.notes.push(format!("no process {pid}"));
        }
    } else if local && Path::new("/proc").is_dir() {
        let port = g.zone.port();
        match peers::listener(port) {
            Listener::Pid(pid) => d.process = read_process(pid, &format!("listens on :{port}")),
            Listener::Hidden => d.notes.push(format!(
                "port {port} belongs to another user's process: run quai-dash as the node's user (or root) for logs and peers"
            )),
            Listener::None => d.notes.push(format!("nothing listens on :{port} here")),
        }
        if d.process.is_none() {
            if let Some(&pid) = by_name().first() {
                d.process = read_process(pid, "process name");
            }
        }
    }
    if let Some(p) = &d.process {
        if p.cwd.is_none() {
            d.notes.push(format!(
                "{} (pid {}) is not readable by this user",
                p.name, p.pid
            ));
        }
    }
    // Logs.
    if g.logs.is_none() {
        d.logs = d.process.as_ref().and_then(Process::logs_dir);
    }
    // Kind, strongest evidence first.
    let found = |k: NodeKind| k != NodeKind::Unknown;
    if let Some(k) = g.kind {
        d.kind = k;
        d.kind_why = "set".into();
    }
    if !found(d.kind) {
        if let Some(p) = d.process.as_ref().filter(|p| found(p.kind())) {
            d.kind = p.kind();
            d.kind_why = format!("process {} (pid {})", p.name, p.pid);
        }
    }
    if !found(d.kind) {
        let dir = g
            .logs
            .map(Path::to_path_buf)
            .or_else(|| d.logs.clone())
            .filter(|p| p.is_dir());
        if let Some((k, why)) = dir.as_deref().and_then(kind_from_logs) {
            d.kind = k;
            d.kind_why = why;
        }
    }
    // Stratum.
    let quick = Duration::from_millis(1500);
    let mut stats = None;
    if !g.stratum {
        let mut urls: Vec<String> = d
            .process
            .as_ref()
            .and_then(Process::stratum_api)
            .into_iter()
            .collect();
        urls.push(format!("http://{}:3336", host_for_url(g.zone.host())));
        for u in urls {
            let Ok(ep) = Endpoint::new(&u, quick) else {
                continue;
            };
            if let Ok(v) = ep.get_json("/api/pool/stats") {
                if v.get("sharesValid").is_some() || v.get("workersTotal").is_some() {
                    d.stratum = Some(u);
                    stats = Some(v);
                    break;
                }
            }
        }
    }
    if !found(d.kind) {
        if let Some(v) = &stats {
            d.kind = kind_from_stratum(v);
            d.kind_why = if d.kind == NodeKind::RsQuai {
                "stratum API reports `mined`".into()
            } else {
                "stratum API without `mined`".into()
            };
        }
    }
    let zone = Endpoint::new(&g.zone.url, quick);
    if !found(d.kind) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "quai_blockNumber", "params": []});
        if let Ok(r) = zone
            .as_ref()
            .map_err(|e| e.clone())
            .and_then(|z| z.post_reply(&body.to_string()))
        {
            d.kind = kind_from_header_names(&r.headers);
            if found(d.kind) {
                d.kind_why = "RPC header casing (weak)".into();
            }
        }
    }
    if !found(d.kind) {
        d.kind_why = "no process, logs or stratum to tell".into();
    }
    // The zone, for go-quai's per-chain log.
    if d.kind != NodeKind::RsQuai {
        if let Ok(z) = &zone {
            if let Ok(Value::Array(l)) = z.call("quai_nodeLocation", json!([])) {
                d.location = Some((
                    l.first().map(hex_u64).unwrap_or(0),
                    l.get(1).map(hex_u64).unwrap_or(0),
                ));
            }
        }
    }
    d
}

fn host_for_url(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn names() {
        assert_eq!(kind_from_name("rs-quai"), NodeKind::RsQuai);
        assert_eq!(
            kind_from_name("/mnt/server/rs-quai/node/bin/rs-quai"),
            NodeKind::RsQuai
        );
        assert_eq!(kind_from_name("rs-quai.prev-671c834"), NodeKind::RsQuai);
        assert_eq!(kind_from_name("go-quai"), NodeKind::GoQuai);
        assert_eq!(kind_from_name("./build/bin/go-quai"), NodeKind::GoQuai);
        assert_eq!(kind_from_name("quai-dash"), NodeKind::Unknown);
        assert_eq!(kind_from_name("bash"), NodeKind::Unknown);
    }

    #[test]
    fn log_layouts() {
        let go = s(&["global.log", "prime.log", "region-0.log", "zone-0-0.log"]);
        assert_eq!(
            kind_from_log_names(&go).map(|k| k.0),
            Some(NodeKind::GoQuai)
        );
        let rs = s(&["global.log", "global-2026-10-02T18-21-21.573.log"]);
        assert_eq!(kind_from_log_names(&rs), None);
        let rs_text =
            "2026-10-08T07:38:36.745678Z  INFO rsq_chain::slice: Best PH pick location=cyprus1\n";
        assert_eq!(kind_from_log_text(rs_text), Some(NodeKind::RsQuai));
        // go-quai's global.log, coloured; a cut first line is skipped.
        let go_text = "ed block\n\u{1b}[36mINFO   \u{1b}[0m[10-08|07:38:36.745] Global logger started                        \u{1b}[36mpath\u{1b}[0m=nodelogs/global.log\n";
        assert_eq!(kind_from_log_text(go_text), Some(NodeKind::GoQuai));
        assert_eq!(kind_from_log_text("hello\nworld\n"), None);
    }

    #[test]
    fn log_dir_on_disk() {
        let dir = std::env::temp_dir().join(format!("quai-dash-detect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join("global.log"),
            "2026-10-01T17:23:03.275792Z  INFO rsq_node::node: network peers=72\n",
        );
        assert_eq!(kind_from_logs(&dir).map(|k| k.0), Some(NodeKind::RsQuai));
        let _ = std::fs::write(dir.join("zone-0-0.log"), "");
        assert_eq!(kind_from_logs(&dir).map(|k| k.0), Some(NodeKind::GoQuai));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stratum_and_headers() {
        let rs = json!({"workersTotal": 7, "sharesValid": 141, "mined": {"prime": 0, "zone": 0}});
        let go = json!({"workersTotal": 7, "sharesValid": 141});
        assert_eq!(kind_from_stratum(&rs), NodeKind::RsQuai);
        assert_eq!(kind_from_stratum(&go), NodeKind::GoQuai);
        let h = |k: &str| vec![(k.to_string(), "application/json".to_string())];
        assert_eq!(kind_from_header_names(&h("Content-Type")), NodeKind::GoQuai);
        assert_eq!(kind_from_header_names(&h("content-type")), NodeKind::RsQuai);
        assert_eq!(
            kind_from_header_names(&h("CONTENT-TYPE")),
            NodeKind::Unknown
        );
        assert_eq!(kind_from_header_names(&[]), NodeKind::Unknown);
    }

    #[test]
    fn command_lines() {
        let rs = s(&[
            "./bin/rs-quai",
            "start",
            "--node.port=4002",
            "--node.stratum-enabled",
            "--node.stratum-api-addr=127.0.0.1:3336",
            "-d",
            "/data/quai",
        ]);
        assert_eq!(
            flag_value(&rs, "--global.data-dir", Some("-d")).as_deref(),
            Some("/data/quai")
        );
        assert_eq!(
            flag_value(&rs, "--node.port", None).as_deref(),
            Some("4002")
        );
        assert_eq!(flag_value(&rs, "--rpc.http-port", None), None);
        let p = Process {
            name: "rs-quai".into(),
            args: rs,
            ..Default::default()
        };
        assert_eq!(p.kind(), NodeKind::RsQuai);
        assert_eq!(p.stratum_api().as_deref(), Some("http://127.0.0.1:3336"));
        let go = Process {
            name: "go-quai".into(),
            args: s(&["go-quai", "start", "--node.stratum-api-addr", ":3336"]),
            ..Default::default()
        };
        assert_eq!(go.stratum_api().as_deref(), Some("http://127.0.0.1:3336"));
        assert_eq!(go.kind(), NodeKind::GoQuai);
    }

    #[test]
    fn this_process() {
        // Our own /proc entry is always readable.
        let p = read_process(std::process::id(), "test");
        assert!(p.is_some_and(|p| p.cwd.is_some() && !p.name.is_empty()));
    }
}
