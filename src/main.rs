//! quai-dash: a live monitor for a Quai node (rs-quai or go-quai), in the
//! browser (`web`) or the terminal (`tui`), with two looks: GHOST and ANGEL.
//!
//! Everything comes from what every node already offers: the zone, region
//! and prime JSON-RPC endpoints, the `nodelogs` directory, and (on the
//! node's host, Linux) the node process's TCP connections for the peer
//! map; and, for the mining view, the node's stratum API.
// Float math and std ln/sin are fine for drawing; the workspace bans them
// for consensus code.
#![allow(clippy::float_arithmetic, clippy::disallowed_methods)]

mod collect;
mod config;
mod demo;
mod detect;
mod geo;
mod logs;
mod peers;
mod raster;
mod rpc;
mod state;
mod stratum;
mod term;
mod tui;
mod web;
mod world;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use serde::Deserialize;

use crate::config::{GeoMode, Partial, Settings, Source};
use crate::detect::{Detected, Given, KindChoice, NodeKind};
use crate::geo::Geo;
use crate::rpc::Endpoint;
use crate::state::{Place, State};

/// Visual theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    /// Cyan cyberbrain HUD.
    Ghost,
    /// Orange command-center alarms.
    Angel,
}

/// Image support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Graphics {
    /// On in kitty, Ghostty and WezTerm.
    Auto,
    /// Always.
    On,
    /// Never (braille map).
    Off,
}

#[derive(Parser)]
#[command(
    name = "quai-dash",
    version,
    about = "Live web and terminal monitor for Quai nodes (rs-quai or go-quai)",
    long_about = "Live web and terminal monitor for Quai nodes (rs-quai or go-quai).\n\n\
        Run it on the node's host with no flags: it finds the node listening on the \
        zone RPC port (127.0.0.1:9200), works out whether it is rs-quai or go-quai, \
        follows its nodelogs, maps its peers and, if the node runs one, watches its \
        stratum. Every option can also come from a QUAI_DASH_* environment variable \
        or the config file (precedence: flag > env > file > detected > default); \
        `quai-dash config` shows what is in effect and why.\n\n\
        Peer locations come from a local GeoLite2/GeoIP2 City database when one is \
        found, otherwise from ip-api.com, which then receives the peers' IP \
        addresses. --geoip off turns this off."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Config file (TOML) [default: the first of
    /// $XDG_CONFIG_HOME/quai-dash/config.toml, ~/.config/quai-dash/config.toml,
    /// /etc/quai-dash/config.toml].
    #[arg(long, global = true, env = "QUAI_DASH_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(flatten)]
    opts: Partial,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the web dashboard (127.0.0.1:8090 unless --listen says otherwise).
    Web,
    /// Run the terminal dashboard.
    Tui,
    /// Show the effective settings, where each came from, and what
    /// detection found.
    Config,
    /// Record a scripted tour of the terminal dashboard as JSON frames
    /// (for the web player).
    #[command(hide = true)]
    Record {
        /// Output file.
        #[arg(long)]
        out: PathBuf,
        /// Terminal size, COLSxROWS.
        #[arg(long, default_value = "150x46")]
        size: String,
        /// Frames per second kept.
        #[arg(long, default_value_t = 12)]
        fps: u64,
        /// Length in seconds.
        #[arg(long, default_value_t = 48)]
        seconds: u64,
    },
}

fn endpoint(url: &str) -> Result<Endpoint, String> {
    Endpoint::new(url, Duration::from_secs(4))
}

/// `off`, `none` or empty: turned off.
fn is_off(v: &str) -> bool {
    matches!(v.trim(), "" | "off" | "none")
}

/// What detection adds as a settings layer, with a note per key.
fn detected_layer(d: &Detected, pre: &Settings) -> (Partial, BTreeMap<&'static str, String>) {
    let mut p = Partial::default();
    let mut how = BTreeMap::new();
    if !pre.explicit("node_kind") {
        let choice = match d.kind {
            NodeKind::RsQuai => Some(KindChoice::RsQuai),
            NodeKind::GoQuai => Some(KindChoice::GoQuai),
            NodeKind::Unknown => None,
        };
        if choice.is_some() {
            p.node_kind = choice;
            how.insert("node_kind", d.kind_why.clone());
        }
    }
    if let Some(proc_) = &d.process {
        p.node_pid = Some(proc_.pid);
        how.insert("node_pid", format!("{} ({})", proc_.name, proc_.how));
    }
    if let Some(l) = &d.logs {
        p.logs = Some(l.clone());
        how.insert("logs", "the node's working directory".into());
    }
    if let Some(u) = &d.stratum {
        p.stratum_api = Some(u.clone());
        how.insert("stratum_api", "answers".into());
    }
    if pre.v.geoip != Some(GeoMode::Off) && !pre.explicit("geoip_db") {
        let dirs = geo::db_dirs();
        if let Some(db) = geo::find_db(&dirs) {
            how.insert(
                "geoip_db",
                format!(
                    "found in {}",
                    db.parent()
                        .map_or(String::new(), |d| d.display().to_string())
                ),
            );
            p.geoip_db = Some(db);
        }
    }
    (p, how)
}

/// Builds the geolocation source the settings ask for, and a description.
fn geo_from(s: &Settings) -> Result<(Geo, String), String> {
    let db = s.v.geoip_db.as_deref().map(config::expand_home);
    let online = || -> Result<(Geo, String), String> {
        Ok((
            Geo::online()?,
            "online (ip-api.com: peer IP addresses are sent there)".into(),
        ))
    };
    match s.v.geoip.unwrap_or(GeoMode::Auto) {
        GeoMode::Off => Ok((Geo::Off, "off".into())),
        GeoMode::Online => online(),
        GeoMode::Db => {
            let db = db.ok_or("--geoip db: no City database found; give --geoip-db FILE")?;
            Ok((geo::open_db(&db)?, format!("db {}", db.display())))
        }
        GeoMode::Auto => match db {
            Some(db) => match geo::open_db(&db) {
                Ok(g) => Ok((g, format!("db {}", db.display()))),
                // An explicit but broken database is an error; a found one
                // falls back to online.
                Err(e) if s.explicit("geoip_db") => Err(e),
                Err(e) => {
                    eprintln!("quai-dash: {e}; using ip-api.com instead");
                    online()
                }
            },
            None => online(),
        },
    }
}

/// `quai-dash config`.
fn print_config(
    s: &Settings,
    file: Option<&Path>,
    d: Option<&Detected>,
    kind: NodeKind,
    log_file: Option<&Path>,
    geo: &str,
) {
    use std::fmt::Write as _;
    use std::io::Write as _;
    let mut o = String::new();
    match file {
        Some(f) => {
            let _ = writeln!(o, "config file  {}", f.display());
        }
        None => {
            let places: Vec<String> = config::config_candidates()
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            let _ = writeln!(o, "config file  none (looked for {})", places.join(", "));
        }
    }
    match d {
        Some(d) => {
            let _ = writeln!(o, "node kind    {} ({})", kind.name(), kind_how(s, d));
            if let Some(p) = &d.process {
                let cwd = p
                    .cwd
                    .as_ref()
                    .map_or("not readable".to_string(), |c| c.display().to_string());
                let _ = writeln!(
                    o,
                    "process      {} pid {} ({}), cwd {cwd}",
                    p.name, p.pid, p.how
                );
            }
            for n in &d.notes {
                let _ = writeln!(o, "note         {n}");
            }
        }
        None => {
            let _ = writeln!(o, "node kind    demo (no detection)");
        }
    }
    let _ = writeln!(
        o,
        "log file     {}",
        log_file.map_or("none".to_string(), |f| f.display().to_string())
    );
    let _ = writeln!(o, "geolocation  {geo}");
    o.push('\n');
    let rows = s.rows();
    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let vw = rows
        .iter()
        .map(|r| r.1.chars().count())
        .max()
        .unwrap_or(0)
        .min(48);
    let _ = writeln!(o, "{:w$}  {:vw$}  SOURCE", "KEY", "VALUE");
    for (k, v, src) in rows {
        let _ = writeln!(o, "{k:w$}  {v:vw$}  {src}");
    }
    o.push('\n');
    let _ = writeln!(
        o,
        "Precedence: flag > QUAI_DASH_* env > config file > detected > default."
    );
    // A closed pipe (`| head`) is not an error worth a panic.
    let _ = std::io::stdout().write_all(o.as_bytes());
}

/// `--explorer` as the base the web page appends `/address/0x…` to: no
/// trailing slash, empty for `off` or anything that is not an http(s) URL.
fn explorer_base(v: Option<&str>) -> String {
    let v = v
        .unwrap_or(config::DEFAULT_EXPLORER)
        .trim()
        .trim_end_matches('/');
    if v.eq_ignore_ascii_case("off") || v.is_empty() {
        return String::new();
    }
    if !(v.starts_with("https://") || v.starts_with("http://")) {
        eprintln!("quai-dash: note: --explorer {v:?} is not an http(s) URL; address links are off");
        return String::new();
    }
    v.to_string()
}

/// Why the node kind is what it is.
fn kind_how(s: &Settings, d: &Detected) -> String {
    match s.source("node_kind") {
        Some(Source::Detected(how)) => how.clone(),
        Some(src) if src.is_explicit() => format!("set by {src}"),
        _ => d.kind_why.clone(),
    }
}

fn run() -> Result<(), String> {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).map_err(|e| e.to_string())?;
    let (flag, env) = cli.opts.split_cli(&matches);
    let file_path = config::config_path(cli.config.as_deref())?;
    let file = match &file_path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            Partial::from_toml(&text).map_err(|e| format!("{}: {e}", p.display()))?
        }
        None => Partial::default(),
    };
    let none = Partial::default();
    let pre = Settings::merge(&flag, &env, &file, &none, &BTreeMap::new());
    let demo = pre.v.demo == Some(true) || matches!(cli.cmd, Cmd::Record { .. });
    // Detection fills what the user left open.
    let detected = if demo {
        None
    } else {
        let zone = endpoint(pre.rpc())?;
        let logs = pre.v.logs.as_deref().map(config::expand_home);
        let stratum_given = pre.v.stratum_api.is_some();
        Some(detect::detect(&Given {
            zone: &zone,
            kind: pre.v.node_kind.and_then(KindChoice::forced),
            pid: pre.v.node_pid,
            logs: logs.as_deref(),
            stratum: stratum_given,
        }))
    };
    let s = match &detected {
        Some(d) => {
            let (layer, how) = detected_layer(d, &pre);
            Settings::merge(&flag, &env, &file, &layer, &how)
        }
        None => pre,
    };
    let kind =
        s.v.node_kind
            .and_then(KindChoice::forced)
            .unwrap_or(NodeKind::Unknown);
    let zone = endpoint(s.rpc())?;
    let region = s.v.region.as_deref().map(endpoint).transpose()?;
    let prime = s.v.prime.as_deref().map(endpoint).transpose()?;
    let compare = match &s.v.compare {
        Some(c) => {
            let (label, url) = c
                .split_once("=http")
                .map_or(("COMPARE".to_string(), c.clone()), |(l, rest)| {
                    (l.to_string(), format!("http{rest}"))
                });
            Some((endpoint(&url)?, label))
        }
        None => None,
    };
    let log_file = match s.v.logs.as_deref() {
        Some(p) if !is_off(&p.to_string_lossy()) => {
            let location = detected.as_ref().and_then(|d| d.location);
            Some(logs::resolve(&config::expand_home(p), kind, location))
        }
        _ => None,
    };
    if matches!(cli.cmd, Cmd::Config) {
        let geo = geo_from(&s).map_or_else(|e| format!("error: {e}"), |g| g.1);
        print_config(
            &s,
            file_path.as_deref(),
            detected.as_ref(),
            kind,
            log_file.as_deref(),
            &geo,
        );
        return Ok(());
    }
    let (geo, geo_text) = if demo {
        (Geo::Off, "demo".to_string())
    } else {
        geo_from(&s)?
    };
    let here = match &s.v.here {
        Some(h) => {
            let (a, b) = h.split_once(',').ok_or("--here expects LAT,LON")?;
            let lat = a.trim().parse().map_err(|_| "--here: bad latitude")?;
            let lon = b.trim().parse().map_err(|_| "--here: bad longitude")?;
            Some(Place {
                lat,
                lon,
                city: String::new(),
                country: String::new(),
            })
        }
        None => None,
    };
    let stratum = match s.v.stratum_api.as_deref() {
        Some(u) if !is_off(u) => Some(endpoint(u)?),
        _ => None,
    };
    let state = Arc::new(Mutex::new(State::default()));
    if let Some(file) = log_file.clone() {
        if let Ok(mut st) = state.lock() {
            st.node.log_file = Some(file.display().to_string());
        }
        let s = state.clone();
        std::thread::spawn(move || logs::follow(file, kind, s));
    }
    if demo {
        if let Ok(mut st) = state.lock() {
            st.node.explorer = explorer_base(s.v.explorer.as_deref());
        }
        let s = state.clone();
        std::thread::spawn(move || demo::run(s));
    } else {
        // What was found, on one line, and anything worth knowing.
        let d = detected.clone().unwrap_or_default();
        let how = kind_how(&s, &d);
        let pid = d
            .process
            .as_ref()
            .filter(|_| !how.contains("pid"))
            .map_or(String::new(), |p| format!(", pid {}", p.pid));
        eprintln!(
            "quai-dash: {} node ({how}{pid}) at {}",
            kind.name(),
            zone.url
        );
        eprintln!(
            "quai-dash: logs {} · stratum {} · geo {geo_text}",
            log_file
                .as_ref()
                .map_or("none".to_string(), |f| f.display().to_string()),
            stratum.as_ref().map_or("none", |e| e.url.as_str()),
        );
        for n in &d.notes {
            eprintln!("quai-dash: note: {n}");
        }
        let cfg = collect::Config {
            label: s.v.label.clone().unwrap_or_default(),
            explorer: explorer_base(s.v.explorer.as_deref()),
            kind,
            kind_how: kind_how(&s, &d),
            zone,
            region,
            prime,
            compare,
            geo,
            here,
            stall_secs: s.v.stall_secs.unwrap_or(60),
            stratum,
            // A PID the user gave pins the peer map; a detected one would
            // go stale when the node restarts, so the port is asked again.
            pid: s.v.node_pid.filter(|_| s.explicit("node_pid")),
        };
        let st = state.clone();
        std::thread::spawn(move || collect::run(cfg, st));
    }
    match cli.cmd {
        Cmd::Web => {
            let listen =
                s.v.listen
                    .clone()
                    .unwrap_or_else(|| config::DEFAULT_LISTEN.into());
            if !config::is_loopback_listen(&listen) {
                eprintln!(
                    "quai-dash: warning: listening on {listen}, beyond this host; the dashboard has no authentication, and the node's logs, peers and miners become visible to anyone who can reach it"
                );
            }
            eprintln!("quai-dash: open http://{listen}/");
            web::serve(&listen, state)
        }
        Cmd::Tui => {
            let term = term::detect();
            let graphics = match s.v.graphics.unwrap_or(Graphics::Auto) {
                Graphics::Auto => term.graphics(),
                Graphics::On => true,
                Graphics::Off => false,
            };
            tui::run(
                state,
                s.v.theme.unwrap_or(Theme::Ghost),
                tui::Options {
                    graphics,
                    kind: term,
                    notify: s.v.notify == Some(true),
                },
            )
        }
        Cmd::Config => Ok(()),
        Cmd::Record {
            out,
            size,
            fps,
            seconds,
        } => {
            let (w, h) = size.split_once('x').ok_or("--size expects COLSxROWS")?;
            let dims = (
                w.parse().map_err(|_| "bad width")?,
                h.parse().map_err(|_| "bad height")?,
            );
            // Let the demo node fill in, then tour both looks and the views.
            std::thread::sleep(Duration::from_millis(300));
            let script = [
                (9.0, 'm', "m  peer map, full screen"),
                (15.0, 'm', "m  back to the dashboard"),
                (20.0, 't', "t  switch look: ANGEL"),
                (24.5, 's', "s  stratum: miners and workers on this node"),
                (28.5, 's', "s  back to the dashboard"),
                (31.0, 'l', "l  node log, full height"),
                (36.0, 'l', "l  back to the dashboard"),
                (40.0, '?', "?  help"),
                (43.0, '?', "?  close help"),
                (44.5, 't', "t  switch look: GHOST"),
            ];
            let json = tui::record(&state, Theme::Ghost, dims, fps, seconds, &script)?;
            std::fs::write(&out, json.to_string())
                .map_err(|e| format!("{}: {e}", out.display()))?;
            eprintln!("quai-dash: wrote {}", out.display());
            Ok(())
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("quai-dash: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::explorer_base;

    #[test]
    fn explorer_base_normalises_or_turns_off() {
        assert_eq!(explorer_base(None), "https://explorer.qu.ai");
        assert_eq!(
            explorer_base(Some("https://orchard.qu.ai/")),
            "https://orchard.qu.ai"
        );
        assert_eq!(explorer_base(Some("off")), "");
        assert_eq!(explorer_base(Some("")), "");
        assert_eq!(explorer_base(Some("javascript:alert(1)")), "");
    }
}
