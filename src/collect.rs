//! Polls the node and keeps [`State`] current: zone blocks every second
//! (catching up block by block), prime/region heads, mining info, pending
//! workshares, peers, and an optional comparison node.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::peers::{self, Geo};
use crate::rpc::{Endpoint, hex_dec, hex_u64, wei_to_quai};
use crate::state::{
    Algo, BLOCK_HISTORY, BlockInfo, Compare, Head, Mining, Peer, PendingShares, Place, State,
    location_name, now_ms,
};
use crate::stratum;

/// What to watch.
pub struct Config {
    /// Display name.
    pub label: String,
    /// Zone RPC.
    pub zone: Endpoint,
    /// Region RPC.
    pub region: Option<Endpoint>,
    /// Prime RPC.
    pub prime: Option<Endpoint>,
    /// Comparison node's zone RPC and label.
    pub compare: Option<(Endpoint, String)>,
    /// Peer geolocation.
    pub geo: Geo,
    /// Where this node is on the map.
    pub here: Option<Place>,
    /// Seconds without a new zone block before a stall event.
    pub stall_secs: u64,
    /// The node's stratum API.
    pub stratum: Option<Endpoint>,
}

fn block_info(b: &Value) -> Option<BlockInfo> {
    let wo = b.get("woHeader")?;
    let h = b.get("header")?;
    let nums = h.get("number").and_then(Value::as_array);
    let num_at = |i: usize| nums.and_then(|n| n.get(i)).map(hex_u64).unwrap_or(0);
    Some(BlockInfo {
        number: hex_u64(&wo["number"]),
        hash: b["hash"]
            .as_str()
            .or_else(|| wo["hash"].as_str())
            .unwrap_or("")
            .to_string(),
        parent: wo["parentHash"].as_str().unwrap_or("").to_string(),
        timestamp: hex_u64(&wo["timestamp"]),
        seen_ms: now_ms(),
        txs: b["transactions"].as_array().map_or(0, |a| a.len() as u32),
        etxs: b["outboundEtxs"].as_array().map_or(0, |a| a.len() as u32),
        workshares: b["workshares"].as_array().map_or(0, |a| a.len() as u32),
        gas_used: hex_u64(&h["gasUsed"]),
        gas_limit: hex_u64(&h["gasLimit"]),
        base_fee: hex_dec(&h["baseFeePerGas"]),
        order: b["order"].as_u64().unwrap_or(2) as u8,
        difficulty: hex_dec(&wo["difficulty"]),
        coinbase: wo["primaryCoinbase"].as_str().unwrap_or("").to_string(),
        auxpow: wo["nonce"]
            .as_str()
            .is_some_and(|n| n.trim_start_matches("0x").chars().all(|c| c == '0')),
        prime_number: num_at(0),
        region_number: num_at(1),
        exchange_rate: hex_dec(&h["exchangeRate"]),
        size: hex_u64(&b["size"]),
        ours: 0,
        ours_block: false,
        ws_coinbases: ws_field(b, "primaryCoinbase"),
        ws_hashes: ws_field(b, "hash"),
    })
}

/// One string field of every workshare a block carries.
fn ws_field(b: &Value, key: &str) -> Vec<String> {
    b["workshares"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w[key].as_str().map(str::to_string))
        .collect()
}

/// The latest block of a prime (`ctx` 0) or region (1) endpoint, numbered
/// in its own chain.
fn head(ep: &Endpoint, ctx: usize) -> Option<Head> {
    let b = ep
        .call("quai_getBlockByNumber", json!(["latest", false]))
        .ok()?;
    let wo = b.get("woHeader")?;
    let own = b["header"]["number"]
        .as_array()
        .and_then(|n| n.get(ctx))
        .map(hex_u64);
    Some(Head {
        number: own.unwrap_or_else(|| hex_u64(&wo["number"])),
        hash: b["hash"].as_str().unwrap_or("").to_string(),
        timestamp: hex_u64(&wo["timestamp"]),
    })
}

fn algo(m: &Value, rate: &str, diff: &str, time: &str) -> Algo {
    let hashrate = match &m[rate] {
        Value::String(s) => s.parse().unwrap_or(0.0),
        v => v.as_f64().unwrap_or(0.0),
    };
    Algo {
        hashrate,
        difficulty: hex_dec(&m[diff]),
        share_time: m[time].as_f64().unwrap_or(0.0),
    }
}

fn mining(m: &Value) -> Mining {
    Mining {
        avg_block_time: m["avgBlockTime"].as_f64().unwrap_or(0.0),
        blocks_analyzed: m["blocksAnalyzed"].as_u64().unwrap_or(0),
        kawpow: algo(
            m,
            "kawpowHashRate",
            "kawpowDifficulty",
            "avgKawpowShareTime",
        ),
        sha: algo(m, "shaHashRate", "shaDifficulty", "avgShaShareTime"),
        scrypt: algo(
            m,
            "scryptHashRate",
            "scryptDifficulty",
            "avgScryptShareTime",
        ),
        base_block_reward: wei_to_quai(&hex_dec(&m["baseBlockReward"]), 4),
        estimated_block_reward: wei_to_quai(&hex_dec(&m["estimatedBlockReward"]), 4),
        workshare_reward: wei_to_quai(&hex_dec(&m["workshareReward"]), 4),
        quai_supply: wei_to_quai(&hex_dec(&m["quaiSupplyTotal"]), 0),
        avg_tx_fees: wei_to_quai(&hex_dec(&m["avgTxFees"]), 4),
    }
}

fn pending(v: &Value) -> PendingShares {
    let mut p = PendingShares::default();
    if let Some(m) = v.as_object() {
        for (k, list) in m {
            let n = list.as_array().map_or(0, |a| a.len() as u32);
            match k.to_ascii_lowercase().as_str() {
                "kawpow" => p.kawpow += n,
                "scrypt" => p.scrypt += n,
                "progpow" => p.progpow += n,
                s if s.starts_with("sha") => p.sha += n,
                _ => {}
            }
        }
    }
    p
}

/// What the stratum events have already announced.
#[derive(Default)]
struct StratumSeen {
    /// Polled at least once (the first poll announces nothing).
    started: bool,
    /// Workshares handed to the node, by hash.
    found: std::collections::HashSet<String>,
    /// Of those, the ones announced as canonical blocks.
    mined: std::collections::HashSet<String>,
}

/// Events for workshares the stratum handed to the node since the last
/// poll, and for those that became the canonical block.
fn stratum_events(seen: &mut StratumSeen, s: &crate::state::Stratum, st: &mut State, now: u64) {
    for f in s.found.iter().rev() {
        let fresh = seen.found.insert(f.hash.clone());
        if fresh && seen.started {
            st.push_event(
                now,
                "workshare",
                format!(
                    "Workshare from {} ({}) at zone {}",
                    short_worker(&f.worker),
                    f.algorithm,
                    f.height
                ),
                Some(f.height),
            );
        }
        if f.status == "block" && seen.mined.insert(f.hash.clone()) && seen.started {
            st.push_event(
                now,
                "mined",
                format!(
                    "Block {} mined through this node by {} ({})",
                    f.height,
                    short_worker(&f.worker),
                    f.algorithm
                ),
                Some(f.height),
            );
        }
    }
    // Keep only what the stratum still lists (it lists newest first, so a
    // dropped hash never comes back).
    let listed: std::collections::HashSet<&String> = s.found.iter().map(|f| &f.hash).collect();
    seen.found.retain(|h| listed.contains(h));
    seen.mined.retain(|h| listed.contains(h));
    seen.started = true;
}

/// `0x00051234…67Fe.gpu0` from `0x00051234AbCd…67Fe.gpu0`.
pub fn short_worker(w: &str) -> String {
    let (addr, name) = w.split_once('.').unwrap_or((w, ""));
    let a = if addr.len() > 14 {
        format!("{}…{}", &addr[..10], &addr[addr.len() - 4..])
    } else {
        addr.to_string()
    };
    if name.is_empty() {
        a
    } else {
        format!("{a}.{name}")
    }
}

/// Runs forever, updating `state`.
pub fn run(cfg: Config, state: Arc<Mutex<State>>) {
    let mut last_zone: u64 = 0;
    let mut last_new = Instant::now();
    let mut stalled = false;
    let mut tick: u64 = 0;
    let mut geo_cache: HashMap<IpAddr, Option<Place>> = HashMap::new();
    let mut first_seen: HashMap<IpAddr, u64> = HashMap::new();
    let mut cmp_hashes: VecDeque<(u64, String)> = VecDeque::new();
    let mut stratum_seen = StratumSeen::default();
    let rpc_ports: Vec<u16> = [Some(&cfg.zone), cfg.region.as_ref(), cfg.prime.as_ref()]
        .into_iter()
        .flatten()
        .map(Endpoint::port)
        .collect();
    let local = matches!(cfg.zone.host(), "127.0.0.1" | "localhost" | "::1" | "[::1]");
    if let Ok(mut st) = state.lock() {
        st.node.label = cfg.label.clone();
        st.node.rpc = cfg.zone.url.clone();
        st.peers.geo = cfg.geo.kind().to_string();
        st.peers.here = cfg.here.clone();
    }
    loop {
        let started = Instant::now();
        let now = now_ms();
        // Zone head and new blocks.
        match cfg.zone.call("quai_blockNumber", json!([])) {
            Ok(v) => {
                let n = hex_u64(&v);
                let was_offline = state.lock().map(|s| !s.node.online).unwrap_or(false);
                if was_offline && tick > 0 {
                    if let Ok(mut st) = state.lock() {
                        st.push_event(now, "online", "Node RPC is back".into(), None);
                    }
                }
                let from = if last_zone == 0 {
                    n.saturating_sub(24)
                } else {
                    last_zone + 1
                };
                let mut fetched = Vec::new();
                for k in from.max(n.saturating_sub(24))..=n {
                    if k <= last_zone && last_zone != 0 {
                        continue;
                    }
                    if let Ok(b) = cfg
                        .zone
                        .call("quai_getBlockByNumber", json!([format!("0x{k:x}"), false]))
                    {
                        if let Some(bi) = block_info(&b) {
                            fetched.push(bi);
                        }
                    }
                }
                if let Ok(mut st) = state.lock() {
                    st.node.online = true;
                    st.node.error = None;
                    let first_fill = last_zone == 0;
                    for bi in fetched {
                        // A new block whose parent isn't our tip: reorg.
                        let reorg = st
                            .blocks
                            .back()
                            .filter(|prev| {
                                bi.number == prev.number + 1
                                    && bi.parent != prev.hash
                                    && !first_fill
                            })
                            .map(|prev| prev.number);
                        if let Some(at) = reorg {
                            st.push_event(
                                now,
                                "reorg",
                                format!("Reorg at zone block {at}"),
                                Some(at),
                            );
                        }
                        if !first_fill && bi.order < 2 {
                            let (kind, text) = if bi.order == 0 {
                                (
                                    "prime",
                                    format!("Prime block {} (zone {})", bi.prime_number, bi.number),
                                )
                            } else {
                                (
                                    "region",
                                    format!(
                                        "Region block {} (zone {})",
                                        bi.region_number, bi.number
                                    ),
                                )
                            };
                            st.push_event(now, kind, text, Some(bi.number));
                        }
                        st.blocks.retain(|b| b.number < bi.number);
                        st.blocks.push_back(bi);
                    }
                    while st.blocks.len() > BLOCK_HISTORY {
                        st.blocks.pop_front();
                    }
                    if n > last_zone {
                        if stalled {
                            st.push_event(now, "resume", format!("Blocks resumed at {n}"), Some(n));
                        }
                        stalled = false;
                        last_new = Instant::now();
                    } else if !stalled && last_new.elapsed() > Duration::from_secs(cfg.stall_secs) {
                        stalled = true;
                        let text = format!("No new zone block for {} s", cfg.stall_secs);
                        st.push_event(now, "stall", text, Some(n));
                    }
                    if let Some(b) = st.blocks.back() {
                        st.chains.zone = Some(Head {
                            number: b.number,
                            hash: b.hash.clone(),
                            timestamp: b.timestamp,
                        });
                    }
                }
                last_zone = last_zone.max(n);
            }
            Err(e) => {
                if let Ok(mut st) = state.lock() {
                    if st.node.online || tick == 0 {
                        st.push_event(now, "offline", format!("Node RPC unreachable: {e}"), None);
                    }
                    st.node.online = false;
                    st.node.error = Some(e);
                }
            }
        }
        let online = state.lock().map(|s| s.node.online).unwrap_or(false);
        if online && tick % 2 == 0 {
            let prime = cfg.prime.as_ref().and_then(|e| head(e, 0));
            let region = cfg.region.as_ref().and_then(|e| head(e, 1));
            if let Ok(mut st) = state.lock() {
                st.chains.prime = prime;
                st.chains.region = region;
            }
        }
        if online && tick % 5 == 0 {
            let m = cfg
                .zone
                .call("quai_getMiningInfo", json!([]))
                .ok()
                .map(|m| mining(&m));
            let p = cfg
                .zone
                .call("quai_getPendingWorkShares", json!([]))
                .ok()
                .map(|v| pending(&v));
            let count = cfg
                .zone
                .call("net_peerCount", json!([]))
                .ok()
                .map(|v| hex_u64(&v) as u32);
            let dir = cfg.zone.call("net_peerCountByDirection", json!([])).ok();
            let loc = if tick == 0 {
                cfg.zone.call("quai_nodeLocation", json!([])).ok()
            } else {
                None
            };
            let chain_id = if tick == 0 {
                cfg.zone
                    .call("quai_chainId", json!([]))
                    .ok()
                    .map(|v| hex_u64(&v))
            } else {
                None
            };
            if let Ok(mut st) = state.lock() {
                if m.is_some() {
                    st.mining = m;
                }
                if let Some(p) = p {
                    st.pending_shares = p;
                }
                if let Some(c) = count {
                    st.peers.count = c;
                }
                if let Some(d) = dir {
                    st.peers.inbound = hex_u64(&d["incoming"]) as u32;
                    st.peers.outbound = hex_u64(&d["outgoing"]) as u32;
                }
                if let Some(l) = loc.as_ref().and_then(Value::as_array) {
                    let r = l.first().map(hex_u64).unwrap_or(0);
                    let z = l.get(1).map(hex_u64).unwrap_or(0);
                    st.node.location = location_name(r, z);
                }
                if chain_id.is_some() {
                    st.node.chain_id = chain_id;
                }
            }
        }
        // Peers from the OS, every 10 s.
        if tick % 10 == 0 {
            let (list, note) = if local {
                match peers::connections(&rpc_ports) {
                    Ok(conns) => {
                        let ips: Vec<IpAddr> = conns.iter().map(|c| c.ip).collect();
                        cfg.geo.resolve(&ips, &mut geo_cache);
                        let list: Vec<Peer> = conns
                            .iter()
                            .map(|c| Peer {
                                ip: c.ip.to_string(),
                                port: c.port,
                                dir: c.dir.to_string(),
                                place: geo_cache.get(&c.ip).cloned().flatten(),
                                since_ms: *first_seen.entry(c.ip).or_insert(now),
                            })
                            .collect();
                        let note = list.is_empty().then(|| "no TCP peers yet".to_string());
                        (list, note)
                    }
                    Err(e) => (Vec::new(), Some(e)),
                }
            } else {
                (
                    Vec::new(),
                    Some("peer map needs quai-dash on the node's host (RPC is remote)".into()),
                )
            };
            if let Ok(mut st) = state.lock() {
                let before = st.peers.list.len();
                let after = list.len();
                if tick > 0 && after > before + 2 {
                    st.push_event(
                        now,
                        "peer",
                        format!("{} new peers ({after} connected)", after - before),
                        None,
                    );
                }
                st.peers.list = list;
                st.peers.note = note;
            }
        }
        // The node's stratum: every 3 s; on-chain settling every tick.
        if let Some(ep) = &cfg.stratum {
            if tick % 3 == 0 {
                let polled = stratum::fetch(ep);
                if let Ok(mut st) = state.lock() {
                    let st = &mut *st;
                    match polled {
                        Ok(p) => {
                            let mut s = stratum::build(&ep.url, &p, st.mining.as_ref());
                            stratum::settle(&mut s, &mut st.blocks);
                            stratum_events(&mut stratum_seen, &s, st, now);
                            st.stratum = Some(s);
                        }
                        Err(e) => {
                            let s = st.stratum.get_or_insert_with(|| crate::state::Stratum {
                                api: ep.url.clone(),
                                ..Default::default()
                            });
                            s.online = false;
                            s.error = Some(e);
                        }
                    }
                }
            } else if let Ok(mut st) = state.lock() {
                let st = &mut *st;
                if let Some(s) = st.stratum.as_mut() {
                    stratum::settle(s, &mut st.blocks);
                }
            }
        }
        // Comparison node: same hash at the same height?
        if let Some((ep, label)) = &cfg.compare {
            let mut cmp = Compare {
                label: label.clone(),
                rpc: ep.url.clone(),
                ..Default::default()
            };
            if let Ok(v) = ep.call("quai_blockNumber", json!([])) {
                cmp.online = true;
                cmp.height = hex_u64(&v);
                let ours: Vec<(u64, String)> = state
                    .lock()
                    .map(|s| {
                        s.blocks
                            .iter()
                            .map(|b| (b.number, b.hash.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                // Check the newest of our blocks it also has and we haven't compared.
                for (n, h) in ours.iter().rev().filter(|(n, _)| *n <= cmp.height).take(3) {
                    if cmp_hashes.iter().any(|(m, _)| m == n) {
                        continue;
                    }
                    if let Ok(b) =
                        ep.call("quai_getBlockByNumber", json!([format!("0x{n:x}"), false]))
                    {
                        let theirs = b["hash"].as_str().unwrap_or("").to_string();
                        cmp_hashes
                            .push_back((*n, if theirs == *h { String::new() } else { theirs }));
                    }
                }
                while cmp_hashes.len() > 64 {
                    cmp_hashes.pop_front();
                }
            }
            cmp.compared = cmp_hashes.len() as u32;
            cmp.matched = cmp_hashes.iter().filter(|(_, d)| d.is_empty()).count() as u32;
            cmp.last_mismatch = cmp_hashes
                .iter()
                .rev()
                .find(|(_, d)| !d.is_empty())
                .map(|(n, _)| *n);
            if let Ok(mut st) = state.lock() {
                let new_mismatch = cmp.last_mismatch.is_some()
                    && st.compare.as_ref().and_then(|c| c.last_mismatch) != cmp.last_mismatch;
                if new_mismatch {
                    let text = format!(
                        "Block {} differs from {}",
                        cmp.last_mismatch.unwrap_or(0),
                        cmp.label
                    );
                    st.push_event(now, "mismatch", text, cmp.last_mismatch);
                }
                st.compare = Some(cmp);
            }
        }
        if let Ok(mut st) = state.lock() {
            st.now_ms = now_ms();
        }
        tick += 1;
        let spent = started.elapsed();
        if spent < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_secs(1) - spent);
        }
    }
}
