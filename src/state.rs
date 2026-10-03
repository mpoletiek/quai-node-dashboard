//! The dashboard's view of a node: everything the web page and the TUI
//! draw, refreshed by the collector and served as JSON at `/api/state`.

use std::collections::VecDeque;

use serde::Serialize;

/// Blocks kept for the block tape and the timing charts.
pub const BLOCK_HISTORY: usize = 96;
/// Log lines kept.
pub const LOG_HISTORY: usize = 400;
/// Events kept.
pub const EVENT_HISTORY: usize = 64;

/// The whole dashboard state.
#[derive(Clone, Debug, Default, Serialize)]
pub struct State {
    /// Unix milliseconds of this snapshot.
    pub now_ms: u64,
    /// The node being watched.
    pub node: NodeInfo,
    /// Prime, region and zone heads.
    pub chains: Chains,
    /// Recent zone blocks, oldest first.
    pub blocks: VecDeque<BlockInfo>,
    /// `quai_getMiningInfo`.
    pub mining: Option<Mining>,
    /// `quai_getPendingWorkShares`, counted per algorithm.
    pub pending_shares: PendingShares,
    /// Peer counts and located peers.
    pub peers: Peers,
    /// A second node compared block by block (for example go-quai next to rs-quai).
    pub compare: Option<Compare>,
    /// Notable moments: prime and region blocks, reorgs, stalls, peers.
    pub events: VecDeque<Event>,
    /// Tail of the node's log file.
    pub logs: VecDeque<LogLine>,
    /// Sequence number of the newest log line (clients ask for `since`).
    pub log_seq: u64,
}

/// Identity and reachability of the watched node.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NodeInfo {
    /// Display name (`--label`).
    pub label: String,
    /// Zone RPC endpoint.
    pub rpc: String,
    /// Location name, e.g. `Cyprus-1`.
    pub location: String,
    /// `quai_chainId`.
    pub chain_id: Option<u64>,
    /// Whether the zone RPC answered on the last poll.
    pub online: bool,
    /// Last RPC error, if any.
    pub error: Option<String>,
    /// Log file being tailed, if any.
    pub log_file: Option<String>,
}

/// Heads of the three chain levels.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Chains {
    /// Prime chain.
    pub prime: Option<Head>,
    /// Region chain.
    pub region: Option<Head>,
    /// Zone chain.
    pub zone: Option<Head>,
}

/// One chain's head.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Head {
    /// Block number.
    pub number: u64,
    /// Block hash.
    pub hash: String,
    /// Block timestamp (unix seconds).
    pub timestamp: u64,
}

/// A zone block as the dashboard shows it.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BlockInfo {
    /// Zone number.
    pub number: u64,
    /// Hash.
    pub hash: String,
    /// Parent hash.
    pub parent: String,
    /// Timestamp (unix seconds).
    pub timestamp: u64,
    /// Unix milliseconds when the dashboard first saw it.
    pub seen_ms: u64,
    /// Transactions (including inbound ETXs).
    pub txs: u32,
    /// Outbound ETXs.
    pub etxs: u32,
    /// Included workshares.
    pub workshares: u32,
    /// Gas used.
    pub gas_used: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Base fee (wei).
    pub base_fee: String,
    /// 0 prime, 1 region, 2 zone.
    pub order: u8,
    /// Difficulty (decimal).
    pub difficulty: String,
    /// Primary coinbase.
    pub coinbase: String,
    /// Mined through AuxPoW (KawPoW era: zero Quai nonce).
    pub auxpow: bool,
    /// Prime block number this block carries.
    pub prime_number: u64,
    /// Region block number this block carries.
    pub region_number: u64,
    /// Exchange rate (Qi per Quai, raw).
    pub exchange_rate: String,
    /// Size in bytes.
    pub size: u64,
}

/// `quai_getMiningInfo`, decoded.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Mining {
    /// Average block time (seconds).
    pub avg_block_time: f64,
    /// Blocks the averages cover.
    pub blocks_analyzed: u64,
    /// KawPoW.
    pub kawpow: Algo,
    /// SHA (BCH/BTC).
    pub sha: Algo,
    /// Scrypt.
    pub scrypt: Algo,
    /// Base block reward (Quai, decimal string).
    pub base_block_reward: String,
    /// Estimated block reward including fees (Quai).
    pub estimated_block_reward: String,
    /// Workshare reward (Quai).
    pub workshare_reward: String,
    /// Total Quai supply (Quai).
    pub quai_supply: String,
    /// Average transaction fees per block (Quai).
    pub avg_tx_fees: String,
}

/// One proof-of-work algorithm.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Algo {
    /// Hashes per second.
    pub hashrate: f64,
    /// Share difficulty (decimal).
    pub difficulty: String,
    /// Average seconds between shares.
    pub share_time: f64,
}

/// Pending workshares per algorithm.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PendingShares {
    /// KawPoW.
    pub kawpow: u32,
    /// SHA.
    pub sha: u32,
    /// Scrypt.
    pub scrypt: u32,
    /// ProgPoW.
    pub progpow: u32,
}

/// Peers.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Peers {
    /// `net_peerCount`.
    pub count: u32,
    /// Inbound (`net_peerCountByDirection`).
    pub inbound: u32,
    /// Outbound.
    pub outbound: u32,
    /// Connected peers seen in the node process's TCP connections.
    pub list: Vec<Peer>,
    /// Geolocation source: `off`, `db` or `online`.
    pub geo: String,
    /// Why the peer list is empty, when it is.
    pub note: Option<String>,
    /// Where this node is, when known (from `--here` or the public IP lookup).
    pub here: Option<Place>,
}

/// One connected peer.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Peer {
    /// Remote IP.
    pub ip: String,
    /// Remote port.
    pub port: u16,
    /// `in` or `out` (best effort).
    pub dir: String,
    /// Location, when geolocation is on.
    pub place: Option<Place>,
    /// Unix milliseconds first seen.
    pub since_ms: u64,
}

/// A point on the map.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Place {
    /// Latitude.
    pub lat: f64,
    /// Longitude.
    pub lon: f64,
    /// City, if known.
    pub city: String,
    /// ISO country code.
    pub country: String,
}

/// The comparison node.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Compare {
    /// Label.
    pub label: String,
    /// RPC endpoint.
    pub rpc: String,
    /// Its zone height.
    pub height: u64,
    /// Whether it answered.
    pub online: bool,
    /// Blocks compared by hash in the window.
    pub compared: u32,
    /// Of those, identical.
    pub matched: u32,
    /// Height of the most recent mismatch, if any.
    pub last_mismatch: Option<u64>,
}

/// A notable moment.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Event {
    /// Unix milliseconds.
    pub t_ms: u64,
    /// `prime`, `region`, `reorg`, `stall`, `resume`, `peer`, `offline`, `online`, `mismatch`.
    pub kind: String,
    /// Human text.
    pub text: String,
    /// Related block number.
    pub number: Option<u64>,
}

/// One log line.
#[derive(Clone, Debug, Default, Serialize)]
pub struct LogLine {
    /// Sequence number.
    pub seq: u64,
    /// `ERROR`, `WARN`, `INFO`, `DEBUG`, `TRACE` or empty.
    pub level: String,
    /// Line with ANSI escapes removed.
    pub text: String,
}

impl State {
    /// Records an event, keeping the newest [`EVENT_HISTORY`].
    pub fn push_event(&mut self, t_ms: u64, kind: &str, text: String, number: Option<u64>) {
        self.events.push_back(Event {
            t_ms,
            kind: kind.to_string(),
            text,
            number,
        });
        while self.events.len() > EVENT_HISTORY {
            self.events.pop_front();
        }
    }

    /// Appends a log line.
    pub fn push_log(&mut self, level: String, text: String) {
        self.log_seq += 1;
        self.logs.push_back(LogLine {
            seq: self.log_seq,
            level,
            text,
        });
        while self.logs.len() > LOG_HISTORY {
            self.logs.pop_front();
        }
    }
}

/// `[region, zone]` → `Cyprus-1` (go-quai's location names).
pub fn location_name(region: u64, zone: u64) -> String {
    const REGIONS: [&str; 3] = ["Cyprus", "Paxos", "Hydra"];
    match REGIONS.get(region as usize) {
        Some(r) => format!("{r}-{}", zone + 1),
        None => format!("Region{region}-Zone{zone}"),
    }
}

/// Unix milliseconds now.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
