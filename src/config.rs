//! Settings and where each one came from. Every option can be set five
//! ways; the first that sets it wins:
//!
//! 1. a command-line flag (`--rpc URL`);
//! 2. a `QUAI_DASH_*` environment variable (`QUAI_DASH_RPC`);
//! 3. the TOML config file (`rpc = "…"`): `--config PATH`, else the first
//!    of `$XDG_CONFIG_HOME/quai-dash/config.toml`,
//!    `~/.config/quai-dash/config.toml` and `/etc/quai-dash/config.toml`;
//! 4. what detection found on this host ([`crate::detect`]);
//! 5. the default.
//!
//! One struct, [`Partial`], is all of these layers: clap fills it from
//! flags and environment (told apart by clap's value source), serde from
//! the file, and detection and the defaults build their own.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use clap::builder::BoolishValueParser;
use clap::parser::ValueSource;
use clap::{ArgMatches, Args, ValueEnum};
use serde::Deserialize;

use crate::detect::KindChoice;
use crate::{Graphics, Theme};

/// The web dashboard's default address: this host only.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8090";
/// The block explorer worker addresses link to (`--explorer`).
pub const DEFAULT_EXPLORER: &str = "https://explorer.qu.ai";
/// The zone RPC's default.
pub const DEFAULT_RPC: &str = "http://127.0.0.1:9200";

/// Where peer locations come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GeoMode {
    /// A local GeoLite2/GeoIP2 City database if one is found, else online.
    Auto,
    /// The local database only.
    Db,
    /// ip-api.com (sends peer IP addresses to that service).
    Online,
    /// No locations.
    Off,
}

/// One layer of settings; `None` leaves a key to the next layer. Also the
/// command line's options (all global, so they go before or after the
/// subcommand) and the config file's keys.
#[derive(Clone, Debug, Default, Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Partial {
    /// Zone JSON-RPC endpoint [default: http://127.0.0.1:9200].
    #[arg(long, global = true, env = "QUAI_DASH_RPC", value_name = "URL")]
    pub rpc: Option<String>,
    /// Region JSON-RPC endpoint [default: the zone host on port 9002].
    #[arg(long, global = true, env = "QUAI_DASH_REGION", value_name = "URL")]
    pub region: Option<String>,
    /// Prime JSON-RPC endpoint [default: the zone host on port 9001].
    #[arg(long, global = true, env = "QUAI_DASH_PRIME", value_name = "URL")]
    pub prime: Option<String>,
    /// Name shown on the dashboard [default: QUAI NODE].
    #[arg(long, global = true, env = "QUAI_DASH_LABEL", value_name = "NAME")]
    pub label: Option<String>,
    /// web: block explorer that worker and miner addresses link to, as
    /// EXPLORER/address/ADDRESS; `off` for no links
    /// [default: https://explorer.qu.ai].
    #[arg(long, global = true, env = "QUAI_DASH_EXPLORER", value_name = "URL")]
    pub explorer: Option<String>,
    /// Node implementation: detected, or forced [default: auto].
    #[arg(long, global = true, env = "QUAI_DASH_NODE_KIND", value_enum)]
    pub node_kind: Option<KindChoice>,
    /// The node's PID, for the peer map [default: whoever listens on the
    /// zone RPC port].
    #[arg(long, global = true, env = "QUAI_DASH_NODE_PID", value_name = "PID")]
    pub node_pid: Option<u32>,
    /// The node's log file or nodelogs directory, or `off` [default: the
    /// node process's nodelogs].
    #[arg(long, global = true, env = "QUAI_DASH_LOGS", value_name = "PATH")]
    pub logs: Option<PathBuf>,
    /// The node's stratum API (`--node.stratum-api-addr`), or `off`, for
    /// the mining view [default: the node's, or port 3336, if it answers].
    #[arg(long, global = true, env = "QUAI_DASH_STRATUM_API", value_name = "URL")]
    pub stratum_api: Option<String>,
    /// A second node's zone RPC to compare blocks with, as URL or LABEL=URL.
    #[arg(
        long,
        global = true,
        env = "QUAI_DASH_COMPARE",
        value_name = "[LABEL=]URL"
    )]
    pub compare: Option<String>,
    /// Peer locations: a local City database if found, else ip-api.com
    /// (`auto`); `db`; `online` (sends peer IP addresses to ip-api.com);
    /// or `off` [default: auto].
    #[arg(long, global = true, env = "QUAI_DASH_GEOIP", value_enum)]
    pub geoip: Option<GeoMode>,
    /// MaxMind GeoLite2/GeoIP2 City database [default: the first found in
    /// /usr/share/GeoIP, /var/lib/GeoIP, ~/.local/share/GeoIP].
    #[arg(long, global = true, env = "QUAI_DASH_GEOIP_DB", value_name = "FILE")]
    pub geoip_db: Option<PathBuf>,
    /// Same as `--geoip online` (kept for older scripts).
    #[arg(
        long,
        global = true,
        env = "QUAI_DASH_GEOIP_ONLINE",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        value_name = "BOOL"
    )]
    pub geoip_online: Option<bool>,
    /// This node's position on the map, as LAT,LON.
    #[arg(long, global = true, env = "QUAI_DASH_HERE", value_name = "LAT,LON")]
    pub here: Option<String>,
    /// Seconds without a zone block before a stall event [default: 60].
    #[arg(long, global = true, env = "QUAI_DASH_STALL_SECS", value_name = "N")]
    pub stall_secs: Option<u64>,
    /// Show an invented node instead of connecting to one.
    #[arg(
        long,
        global = true,
        env = "QUAI_DASH_DEMO",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        value_name = "BOOL"
    )]
    pub demo: Option<bool>,
    /// web: address to listen on [default: 127.0.0.1:8090]. Anything but
    /// loopback exposes the dashboard (no authentication) to the network.
    #[arg(long, global = true, env = "QUAI_DASH_LISTEN", value_name = "ADDR")]
    pub listen: Option<String>,
    /// tui: starting look, `t` toggles [default: ghost].
    #[arg(long, global = true, env = "QUAI_DASH_THEME", value_enum)]
    pub theme: Option<Theme>,
    /// tui: pixel peer map with the kitty graphics protocol (kitty,
    /// Ghostty, WezTerm): auto-detected, or forced [default: auto].
    #[arg(long, global = true, env = "QUAI_DASH_GRAPHICS", value_enum)]
    pub graphics: Option<Graphics>,
    /// tui: desktop notifications for stalls, reorgs, mismatches and RPC
    /// loss.
    #[arg(
        long,
        global = true,
        env = "QUAI_DASH_NOTIFY",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        value_name = "BOOL"
    )]
    pub notify: Option<bool>,
}

/// Calls `$m!` with every key of [`Partial`], in display order.
macro_rules! keys {
    ($m:ident) => {
        $m!(
            rpc,
            region,
            prime,
            label,
            explorer,
            node_kind,
            node_pid,
            logs,
            stratum_api,
            compare,
            geoip,
            geoip_db,
            geoip_online,
            here,
            stall_secs,
            demo,
            listen,
            theme,
            graphics,
            notify
        )
    };
}

impl Partial {
    /// The defaults (region and prime follow the zone; see [`Settings`]).
    pub fn defaults() -> Partial {
        Partial {
            rpc: Some(DEFAULT_RPC.into()),
            label: Some("QUAI NODE".into()),
            explorer: Some(DEFAULT_EXPLORER.into()),
            node_kind: Some(KindChoice::Auto),
            geoip: Some(GeoMode::Auto),
            stall_secs: Some(60),
            demo: Some(false),
            listen: Some(DEFAULT_LISTEN.into()),
            theme: Some(Theme::Ghost),
            graphics: Some(Graphics::Auto),
            notify: Some(false),
            ..Partial::default()
        }
    }

    /// Reads a TOML config file.
    pub fn from_toml(text: &str) -> Result<Partial, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Folds the aliases into their keys: `geoip_online = true` is
    /// `geoip = "online"` unless the same layer sets `geoip`.
    fn normalize(mut self) -> Partial {
        if self.geoip_online == Some(true) && self.geoip.is_none() {
            self.geoip = Some(GeoMode::Online);
        }
        self
    }

    /// Splits what clap parsed into flags and environment variables.
    pub fn split_cli(&self, m: &ArgMatches) -> (Partial, Partial) {
        let mut flag = Partial::default();
        let mut env = Partial::default();
        macro_rules! split {
            ($($k:ident),*) => {$(
                if let Some(v) = &self.$k {
                    if source(m, stringify!($k)) == Some(ValueSource::EnvVariable) {
                        env.$k = Some(v.clone());
                    } else {
                        flag.$k = Some(v.clone());
                    }
                }
            )*};
        }
        keys!(split);
        (flag, env)
    }
}

/// Where an argument's value came from, at the top level or in the
/// subcommand (global arguments can sit on either side of it).
fn source(m: &ArgMatches, id: &str) -> Option<ValueSource> {
    let here = m.try_contains_id(id).ok().and_then(|_| m.value_source(id));
    here.or_else(|| {
        m.subcommand().and_then(|(_, sub)| {
            sub.try_contains_id(id)
                .ok()
                .and_then(|_| sub.value_source(id))
        })
    })
}

/// Where a setting came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A command-line flag.
    Flag,
    /// A `QUAI_DASH_*` environment variable.
    Env,
    /// The config file.
    File,
    /// Detection, and how.
    Detected(String),
    /// The built-in default, with a note.
    Default(Option<String>),
}

impl Source {
    /// Whether the user set it (flag, environment or file).
    pub fn is_explicit(&self) -> bool {
        matches!(self, Source::Flag | Source::Env | Source::File)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Flag => f.write_str("flag"),
            Source::Env => f.write_str("env"),
            Source::File => f.write_str("file"),
            Source::Detected(how) => write!(f, "detected: {how}"),
            Source::Default(None) => f.write_str("default"),
            Source::Default(Some(n)) => write!(f, "default: {n}"),
        }
    }
}

/// How a value prints.
trait Show {
    fn show(&self) -> String;
}

impl Show for String {
    fn show(&self) -> String {
        self.clone()
    }
}

impl Show for PathBuf {
    fn show(&self) -> String {
        self.display().to_string()
    }
}

macro_rules! show_display {
    ($($t:ty),*) => {$(
        impl Show for $t {
            fn show(&self) -> String {
                self.to_string()
            }
        }
    )*};
}
show_display!(u32, u64, bool);

macro_rules! show_value_enum {
    ($($t:ty),*) => {$(
        impl Show for $t {
            fn show(&self) -> String {
                self.to_possible_value()
                    .map(|v| v.get_name().to_string())
                    .unwrap_or_default()
            }
        }
    )*};
}
show_value_enum!(KindChoice, GeoMode, Theme, Graphics);

/// The merged settings.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// The values (every key with a default is set).
    pub v: Partial,
    /// Where each set key came from.
    pub src: BTreeMap<&'static str, Source>,
}

impl Settings {
    /// Merges the layers, highest precedence first. `detected` carries a
    /// note per key for [`Source::Detected`].
    pub fn merge(
        flag: &Partial,
        env: &Partial,
        file: &Partial,
        detected: &Partial,
        detected_how: &BTreeMap<&'static str, String>,
    ) -> Settings {
        let mut s = Settings::default();
        s.layer(&flag.clone().normalize(), |_| Source::Flag);
        s.layer(&env.clone().normalize(), |_| Source::Env);
        s.layer(&file.clone().normalize(), |_| Source::File);
        s.layer(detected, |k| {
            Source::Detected(detected_how.get(k).cloned().unwrap_or_default())
        });
        s.layer(&Partial::defaults(), |_| Source::Default(None));
        // Region and prime default to the zone's host.
        let host = crate::rpc::Endpoint::new(s.rpc(), std::time::Duration::from_secs(1))
            .map(|e| e.host().to_string())
            .unwrap_or_else(|_| "127.0.0.1".into());
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        for (key, port) in [("region", 9002), ("prime", 9001)] {
            let slot = if key == "region" {
                &mut s.v.region
            } else {
                &mut s.v.prime
            };
            if slot.is_none() {
                *slot = Some(format!("http://{host}:{port}"));
                s.src.insert(
                    key,
                    Source::Default(Some(format!("zone host, port {port}"))),
                );
            }
        }
        s
    }

    fn layer(&mut self, p: &Partial, src: impl Fn(&'static str) -> Source) {
        macro_rules! take {
            ($($k:ident),*) => {$(
                if self.v.$k.is_none() {
                    if let Some(v) = &p.$k {
                        self.v.$k = Some(v.clone());
                        self.src.insert(stringify!($k), src(stringify!($k)));
                    }
                }
            )*};
        }
        keys!(take);
    }

    /// Where `key` came from.
    pub fn source(&self, key: &str) -> Option<&Source> {
        self.src.get(key)
    }

    /// Whether the user set `key` (flag, environment or file).
    pub fn explicit(&self, key: &str) -> bool {
        self.source(key).is_some_and(Source::is_explicit)
    }

    /// Every key as `(key, value, source)`; unset keys show `-`. The
    /// `geoip_online` alias is folded into `geoip` and not listed.
    pub fn rows(&self) -> Vec<(&'static str, String, String)> {
        let mut out = Vec::new();
        macro_rules! row {
            ($($k:ident),*) => {$(
                if stringify!($k) != "geoip_online" {
                    out.push((
                        stringify!($k),
                        self.v.$k.as_ref().map_or("-".to_string(), Show::show),
                        self.source(stringify!($k)).map_or("unset".to_string(), ToString::to_string),
                    ));
                }
            )*};
        }
        keys!(row);
        out
    }

    /// The zone RPC.
    pub fn rpc(&self) -> &str {
        self.v.rpc.as_deref().unwrap_or(DEFAULT_RPC)
    }
}

/// The config file to read: the one given (it must exist), else the first
/// of the standard places that exists.
pub fn config_path(given: Option<&Path>) -> Result<Option<PathBuf>, String> {
    if let Some(p) = given {
        return if p.is_file() {
            Ok(Some(p.to_path_buf()))
        } else {
            Err(format!("config file {} not found", p.display()))
        };
    }
    Ok(config_candidates().into_iter().find(|p| p.is_file()))
}

/// The standard config file places, in order.
pub fn config_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        out.push(PathBuf::from(x).join("quai-dash/config.toml"));
    }
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        out.push(PathBuf::from(h).join(".config/quai-dash/config.toml"));
    }
    out.push(PathBuf::from("/etc/quai-dash/config.toml"));
    out.dedup();
    out
}

/// `~/x` → `$HOME/x`.
pub fn expand_home(p: &Path) -> PathBuf {
    match (p.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(h)) => PathBuf::from(h).join(rest),
        _ => p.to_path_buf(),
    }
}

/// Whether a listen address stays on this host.
pub fn is_loopback_listen(addr: &str) -> bool {
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    let host = host.trim_matches(|c| c == '[' || c == ']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use clap::{CommandFactory, FromArgMatches, Parser};

    use super::*;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        opts: Partial,
        #[command(subcommand)]
        cmd: Option<Sub>,
    }

    #[derive(clap::Subcommand)]
    enum Sub {
        Web,
    }

    fn how(pairs: &[(&'static str, &str)]) -> BTreeMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    #[test]
    fn precedence() {
        let flag = Partial {
            rpc: Some("http://flag:9200".into()),
            ..Default::default()
        };
        let env = Partial {
            rpc: Some("http://env:9200".into()),
            label: Some("ENV".into()),
            ..Default::default()
        };
        let file = Partial {
            label: Some("FILE".into()),
            listen: Some("0.0.0.0:8095".into()),
            logs: Some("/file/nodelogs".into()),
            ..Default::default()
        };
        let detected = Partial {
            logs: Some("/detected/nodelogs".into()),
            node_kind: Some(KindChoice::RsQuai),
            stratum_api: Some("http://127.0.0.1:3336".into()),
            ..Default::default()
        };
        let s = Settings::merge(
            &flag,
            &env,
            &file,
            &detected,
            &how(&[("node_kind", "process rs-quai"), ("stratum_api", "answers")]),
        );
        assert_eq!(s.v.rpc.as_deref(), Some("http://flag:9200"));
        assert_eq!(s.source("rpc"), Some(&Source::Flag));
        assert_eq!(s.v.label.as_deref(), Some("ENV"));
        assert_eq!(s.source("label"), Some(&Source::Env));
        assert_eq!(s.v.listen.as_deref(), Some("0.0.0.0:8095"));
        assert_eq!(s.source("listen"), Some(&Source::File));
        assert_eq!(s.v.logs, Some(PathBuf::from("/file/nodelogs")));
        assert!(s.explicit("logs"));
        assert_eq!(s.v.node_kind, Some(KindChoice::RsQuai));
        assert_eq!(
            s.source("node_kind"),
            Some(&Source::Detected("process rs-quai".into()))
        );
        assert!(!s.explicit("stratum_api"));
        assert_eq!(s.v.stall_secs, Some(60));
        assert_eq!(s.source("stall_secs"), Some(&Source::Default(None)));
        assert_eq!(s.v.region.as_deref(), Some("http://flag:9002"));
        assert_eq!(s.v.prime.as_deref(), Some("http://flag:9001"));
        assert!(matches!(s.source("prime"), Some(Source::Default(Some(_)))));
        assert_eq!(s.v.compare, None);
        let rows = s.rows();
        assert!(rows.iter().any(|r| r.0 == "compare" && r.2 == "unset"));
        assert!(!rows.iter().any(|r| r.0 == "geoip_online"));
    }

    #[test]
    fn defaults_bind_loopback() {
        let s = Settings::merge(
            &Partial::default(),
            &Partial::default(),
            &Partial::default(),
            &Partial::default(),
            &BTreeMap::new(),
        );
        assert_eq!(s.v.listen.as_deref(), Some(DEFAULT_LISTEN));
        assert!(is_loopback_listen(DEFAULT_LISTEN));
        assert_eq!(s.v.geoip, Some(GeoMode::Auto));
        assert_eq!(s.v.node_kind, Some(KindChoice::Auto));
        assert!(is_loopback_listen("[::1]:8090"));
        assert!(is_loopback_listen("localhost:8090"));
        assert!(!is_loopback_listen("0.0.0.0:8090"));
        assert!(!is_loopback_listen("10.0.0.13:8095"));
        assert!(!is_loopback_listen("[::]:8090"));
    }

    #[test]
    fn geoip_online_alias() {
        // The alias sets geoip in its own layer...
        let flag = Partial {
            geoip_online: Some(true),
            ..Default::default()
        };
        let file = Partial {
            geoip: Some(GeoMode::Off),
            ..Default::default()
        };
        let s = Settings::merge(
            &flag,
            &Partial::default(),
            &file,
            &Partial::default(),
            &BTreeMap::new(),
        );
        assert_eq!(s.v.geoip, Some(GeoMode::Online));
        assert_eq!(s.source("geoip"), Some(&Source::Flag));
        // ...but not over an explicit geoip in the same layer.
        let both = Partial {
            geoip_online: Some(true),
            geoip: Some(GeoMode::Off),
            ..Default::default()
        };
        let s = Settings::merge(
            &both,
            &Partial::default(),
            &file,
            &Partial::default(),
            &BTreeMap::new(),
        );
        assert_eq!(s.v.geoip, Some(GeoMode::Off));
    }

    #[test]
    fn toml_file() {
        let p = Partial::from_toml(
            r#"
            rpc = "http://127.0.0.1:9200"
            label = "RS-QUAI SOAK"
            node_kind = "rs-quai"
            logs = "~/node/nodelogs"
            geoip = "online"
            stall_secs = 90
            listen = "127.0.0.1:8095"
            theme = "angel"
            notify = true
            "#,
        )
        .unwrap();
        assert_eq!(p.node_kind, Some(KindChoice::RsQuai));
        assert_eq!(p.geoip, Some(GeoMode::Online));
        assert_eq!(p.stall_secs, Some(90));
        assert_eq!(p.theme, Some(Theme::Angel));
        assert_eq!(p.notify, Some(true));
        // Typos are errors, not silently ignored.
        let e = Partial::from_toml("lable = \"x\"").unwrap_err();
        assert!(e.contains("lable"), "{e}");
        assert!(Partial::from_toml("geoip = \"maybe\"").is_err());
        // The shipped example parses.
        let example = include_str!("../contrib/config.toml");
        assert!(Partial::from_toml(example).is_ok());
    }

    #[test]
    fn cli_flags_either_side_of_the_subcommand() {
        let m = T::command()
            .try_get_matches_from([
                "quai-dash",
                "--label",
                "RS-QUAI SOAK",
                "--geoip-online",
                "web",
                "--listen",
                "10.0.0.13:8095",
            ])
            .unwrap();
        let t = T::from_arg_matches(&m).unwrap();
        assert!(matches!(t.cmd, Some(Sub::Web)));
        let (flag, env) = t.opts.split_cli(&m);
        assert_eq!(flag.label.as_deref(), Some("RS-QUAI SOAK"));
        assert_eq!(flag.listen.as_deref(), Some("10.0.0.13:8095"));
        assert_eq!(flag.geoip_online, Some(true));
        assert!(env.listen.is_none() && env.label.is_none());
        // A boolean flag doesn't swallow the subcommand after it.
        let m = T::command()
            .try_get_matches_from(["quai-dash", "--demo", "web", "--notify=false"])
            .unwrap();
        let t = T::from_arg_matches(&m).unwrap();
        assert_eq!(t.opts.demo, Some(true));
        assert_eq!(t.opts.notify, Some(false));
        assert!(matches!(t.cmd, Some(Sub::Web)));
    }

    #[test]
    fn home_paths() {
        if let Some(h) = std::env::var_os("HOME") {
            assert_eq!(
                expand_home(Path::new("~/node/nodelogs")),
                PathBuf::from(h).join("node/nodelogs")
            );
        }
        assert_eq!(expand_home(Path::new("/var/x")), PathBuf::from("/var/x"));
    }
}
