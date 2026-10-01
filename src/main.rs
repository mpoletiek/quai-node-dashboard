//! quai-dash: a live monitor for a Quai node (rs-quai or go-quai), in the
//! browser (`web`) or the terminal (`tui`), with two looks: GHOST and ANGEL.
//!
//! Everything comes from what every node already offers: the zone, region
//! and prime JSON-RPC endpoints, the `nodelogs` directory, and (on the
//! node's host, Linux) the node process's TCP connections for the peer
//! map.
#![allow(clippy::float_arithmetic)]

mod collect;
mod logs;
mod peers;
mod rpc;
mod state;
mod tui;
mod web;
mod world;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use crate::peers::Geo;
use crate::rpc::Endpoint;
use crate::state::{Place, State};

/// Visual theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Theme {
    /// Cyan cyberbrain HUD.
    Ghost,
    /// Orange command-center alarms.
    Angel,
}

#[derive(Parser)]
#[command(name = "quai-dash", version, about = "Live web and terminal monitor for Quai nodes (rs-quai or go-quai)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Zone JSON-RPC endpoint.
    #[arg(long, global = true, default_value = "http://127.0.0.1:9200", env = "QUAI_DASH_RPC")]
    rpc: String,
    /// Region JSON-RPC endpoint (default: the zone host on port 9002).
    #[arg(long, global = true)]
    region: Option<String>,
    /// Prime JSON-RPC endpoint (default: the zone host on port 9001).
    #[arg(long, global = true)]
    prime: Option<String>,
    /// Name shown on the dashboard.
    #[arg(long, global = true, default_value = "QUAI NODE")]
    label: String,
    /// The node's log file or nodelogs directory to follow.
    #[arg(long, global = true)]
    logs: Option<PathBuf>,
    /// A second node's zone RPC to compare blocks with, as URL or LABEL=URL.
    #[arg(long, global = true)]
    compare: Option<String>,
    /// MaxMind GeoLite2/GeoIP2 City database for the peer map.
    #[arg(long, global = true)]
    geoip_db: Option<PathBuf>,
    /// Locate peers with ip-api.com (sends peer IP addresses to that service).
    #[arg(long, global = true)]
    geoip_online: bool,
    /// This node's position on the map, as LAT,LON.
    #[arg(long, global = true)]
    here: Option<String>,
    /// Seconds without a zone block before the dashboard raises a stall.
    #[arg(long, global = true, default_value_t = 60)]
    stall_secs: u64,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the web dashboard.
    Web {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:8090")]
        listen: String,
    },
    /// Run the terminal dashboard.
    Tui {
        /// Starting theme (t toggles).
        #[arg(long, value_enum, default_value_t = Theme::Ghost)]
        theme: Theme,
    },
}

fn endpoint(url: &str) -> Result<Endpoint, String> {
    Endpoint::new(url, Duration::from_secs(4))
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let zone = endpoint(&cli.rpc)?;
    let region = match &cli.region {
        Some(u) => Some(endpoint(u)?),
        None => Some(zone.with_port(9002)),
    };
    let prime = match &cli.prime {
        Some(u) => Some(endpoint(u)?),
        None => Some(zone.with_port(9001)),
    };
    let compare = match &cli.compare {
        Some(c) => {
            let (label, url) = c.split_once("=http").map_or(("COMPARE".to_string(), c.clone()), |(l, rest)| (l.to_string(), format!("http{rest}")));
            Some((endpoint(&url)?, label))
        }
        None => None,
    };
    let geo = match (&cli.geoip_db, cli.geoip_online) {
        (Some(p), _) => peers::open_db(p)?,
        (None, true) => Geo::Online(Endpoint::new("http://ip-api.com/batch", Duration::from_secs(6))?),
        (None, false) => Geo::Off,
    };
    let here = match &cli.here {
        Some(s) => {
            let (a, b) = s.split_once(',').ok_or("--here expects LAT,LON")?;
            let lat = a.trim().parse().map_err(|_| "--here: bad latitude")?;
            let lon = b.trim().parse().map_err(|_| "--here: bad longitude")?;
            Some(Place { lat, lon, city: String::new(), country: String::new() })
        }
        None => None,
    };
    let state = Arc::new(Mutex::new(State::default()));
    if let Some(p) = &cli.logs {
        let file = logs::resolve(p);
        if let Ok(mut st) = state.lock() {
            st.node.log_file = Some(file.display().to_string());
        }
        let s = state.clone();
        std::thread::spawn(move || logs::follow(file, s));
    }
    let cfg = collect::Config { label: cli.label.clone(), zone, region, prime, compare, geo, here, stall_secs: cli.stall_secs };
    {
        let s = state.clone();
        std::thread::spawn(move || collect::run(cfg, s));
    }
    match cli.cmd {
        Cmd::Web { listen } => {
            eprintln!("quai-dash: watching {} — open http://{listen}/", cli.rpc);
            web::serve(&listen, state)
        }
        Cmd::Tui { theme } => tui::run(state, theme),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("quai-dash: {e}");
        std::process::exit(1);
    }
}
