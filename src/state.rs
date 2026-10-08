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
    /// The node's stratum server seen through its API (`--stratum-api`):
    /// every miner and worker connected to this node. `None` when not
    /// configured.
    pub stratum: Option<Stratum>,
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
    /// Block explorer base URL for address links (`--explorer`); empty for
    /// none.
    pub explorer: String,
    /// Zone RPC endpoint.
    pub rpc: String,
    /// Node implementation: `rs-quai`, `go-quai` or `unknown`.
    pub kind: String,
    /// How the kind was found (`process rs-quai (pid 4242)`, `set by flag`, …).
    pub kind_how: String,
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
    /// Workshares in this block paid to an address mining on this node's
    /// stratum.
    pub ours: u32,
    /// Whether the block itself is paid to an address mining on this
    /// node's stratum.
    pub ours_block: bool,
    /// Coinbases of the block's workshares (for the stratum's on-chain
    /// count; not sent to the browser).
    #[serde(skip)]
    pub ws_coinbases: Vec<String>,
    /// Hashes of the block's workshares (not sent to the browser).
    #[serde(skip)]
    pub ws_hashes: Vec<String>,
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

/// The node's stratum server: the miners and workers connected to it, what
/// they submit, and what became workshares and blocks.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Stratum {
    /// API endpoint.
    pub api: String,
    /// Whether the API answered on the last poll.
    pub online: bool,
    /// Last error, when it did not.
    pub error: Option<String>,
    /// Seconds since the stratum started.
    pub uptime: f64,
    /// Workers connected now.
    pub workers_connected: u32,
    /// Workers seen since the stratum started.
    pub workers_total: u32,
    /// Distinct payout addresses behind the connected workers.
    pub miners: u32,
    /// Accepted shares.
    pub shares_valid: u64,
    /// Late shares.
    pub shares_stale: u64,
    /// Rejected shares.
    pub shares_invalid: u64,
    /// Shares that met the workshare target and went to the node.
    pub workshares_found: u64,
    /// KawPoW.
    pub kawpow: StratumAlgo,
    /// SHA-256d.
    pub sha: StratumAlgo,
    /// Scrypt.
    pub scrypt: StratumAlgo,
    /// Connected workers, busiest first.
    pub workers: Vec<StratumWorker>,
    /// Share luck from the recent share history.
    pub luck: ShareLuck,
    /// Recent workshares handed to the node, newest first.
    pub found: Vec<FoundShare>,
    /// What the canonical chain paid to the stratum's miners.
    pub onchain: OnChain,
    /// What the node made of the shares handed to it since the stratum
    /// started (rs-quai's `mined` in `/api/pool/stats`; go-quai has none).
    pub mined: Option<StratumMined>,
}

/// Shares handed to the node, by outcome, since the stratum started.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct StratumMined {
    /// Prime blocks.
    pub prime: u64,
    /// Region blocks.
    pub region: u64,
    /// Zone blocks.
    pub zone: u64,
    /// Kept as workshares.
    pub workshares: u64,
    /// Workshares seen in canonical blocks.
    pub workshares_paid: u64,
}

/// One algorithm on the stratum.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StratumAlgo {
    /// Hashes per second from this node's workers.
    pub hashrate: f64,
    /// Connected workers.
    pub workers: u32,
    /// Valid shares.
    pub shares_valid: u64,
    /// This node's fraction of the network's hashrate (0..1).
    pub network_share: f64,
    /// Expected workshares per hour at that fraction.
    pub expected_per_hour: f64,
}

/// One worker (a rig or miner process) on the stratum.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StratumWorker {
    /// Payout address.
    pub address: String,
    /// Worker name.
    pub name: String,
    /// `kawpow`, `sha256` or `scrypt`.
    pub algorithm: String,
    /// Stratum difficulty of its last share (0 before the first).
    pub difficulty: f64,
    /// Estimated hashes per second.
    pub hashrate: f64,
    /// Valid shares.
    pub valid: u64,
    /// Stale shares.
    pub stale: u64,
    /// Invalid shares.
    pub invalid: u64,
    /// Unix milliseconds of its last share (0: none yet).
    pub last_share_ms: u64,
    /// Unix milliseconds it connected.
    pub connected_ms: u64,
}

/// Share luck (`/api/pool/shares`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct ShareLuck {
    /// Shares in the history.
    pub shares: u64,
    /// Current workshare difficulty, in stratum units.
    pub workshare_diff: f64,
    /// Mean achieved / workshare difficulty, in percent.
    pub average: f64,
    /// Best share, in percent of the workshare difficulty.
    pub best: f64,
    /// Shares expected per workshare at the current difficulties.
    pub expected_shares: f64,
}

/// A workshare the stratum handed to the node.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FoundShare {
    /// Zone height of its template.
    pub height: u64,
    /// Work object hash.
    pub hash: String,
    /// `address.worker`.
    pub worker: String,
    /// Algorithm.
    pub algorithm: String,
    /// Unix milliseconds.
    pub found_ms: u64,
    /// `block` (it is the canonical block at its height), `included` (a
    /// canonical block carries it as a workshare), `pending` (not in the
    /// window yet) or `unseen` (older than the window).
    pub status: String,
}

/// Paid on-chain to the stratum's miners, over the recent block window.
#[derive(Clone, Debug, Default, Serialize)]
pub struct OnChain {
    /// Canonical blocks scanned.
    pub window: u32,
    /// Workshares in those blocks paid to the miners' addresses.
    pub workshares: u32,
    /// Blocks paid to the miners' addresses.
    pub blocks: u32,
    /// Workshares handed to the node that a canonical block includes.
    pub found_included: u32,
    /// Workshares handed to the node that became the canonical block.
    pub found_blocks: u32,
    /// Per miner address.
    pub by_address: Vec<AddressPaid>,
}

/// One miner address on the stratum and what the chain paid it.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AddressPaid {
    /// Address.
    pub address: String,
    /// Connected workers.
    pub workers: u32,
    /// Algorithms its workers mine.
    pub algorithms: Vec<String>,
    /// Workshares paid in the window.
    pub workshares: u32,
    /// Blocks paid in the window.
    pub blocks: u32,
}

/// A notable moment.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Event {
    /// Unix milliseconds.
    pub t_ms: u64,
    /// `prime`, `region`, `reorg`, `stall`, `resume`, `peer`, `offline`,
    /// `online`, `mismatch`, `workshare` (found by this node's stratum) or
    /// `mined` (a block found by this node's stratum).
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

/// `s` as its first `head` and last `tail` characters around `…`, when it
/// is longer than `max` characters. By characters, not bytes: miners name
/// their workers, and a byte cut inside a character would panic.
pub fn abbrev(s: &str, max: usize, head: usize, tail: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(n.saturating_sub(tail)).collect();
    format!("{h}…{t}")
}

/// `[region, zone]` → `Cyprus-1` (go-quai's location names).
pub fn location_name(region: u64, zone: u64) -> String {
    const REGIONS: [&str; 3] = ["Cyprus", "Paxos", "Hydra"];
    match REGIONS.get(region as usize) {
        Some(r) => format!("{r}-{}", zone.saturating_add(1)),
        None => format!("Region{region}-Zone{zone}"),
    }
}

/// Unix milliseconds now.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_text_and_numbers_never_panic() {
        assert_eq!(abbrev("0x0123456789abcdef", 14, 8, 4), "0x012345…cdef");
        assert_eq!(abbrev("short", 14, 8, 4), "short");
        // Multi-byte characters where a byte cut would land inside one.
        assert_eq!(abbrev("0xé€€€€€€€€€€€€€€€é", 14, 8, 4), "0xé€€€€€…€€€é");
        assert_eq!(
            crate::collect::short_worker("0x€€€€€€€€€€€€€€€€€€€€.rig€"),
            "0x€€€€€€€€…€€€€.rig€"
        );
        assert_eq!(location_name(0, u64::MAX), format!("Cyprus-{}", u64::MAX));
    }
}
