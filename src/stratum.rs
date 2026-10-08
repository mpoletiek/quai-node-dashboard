//! The node's stratum server through its HTTP API (rs-quai serves a port of
//! go-quai's, so both answer the same way): the miners and workers mining
//! to this node, their shares, the workshares the stratum handed to the
//! node, and what the canonical chain paid the stratum's miners.
//!
//! Endpoints read: `/api/pool/stats`, `/api/pool/workers`,
//! `/api/pool/shares` and `/api/pool/blocks`. In the stratum's own words a
//! "block found" is any share that met the workshare target and was handed
//! to the node; whether it became the zone block or a workshare in a later
//! block is decided here, against the chain.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde_json::Value;

use crate::rpc::Endpoint;
use crate::state::{
    AddressPaid, BlockInfo, FoundShare, Mining, OnChain, ShareLuck, Stratum, StratumAlgo,
    StratumWorker,
};

/// Workshares handed to the node that are kept and tracked.
const FOUND_KEPT: usize = 32;
/// Workers kept (the fastest): a stratum open to anyone can have any
/// number, and the whole list is copied every frame and every poll.
const WORKERS_KEPT: usize = 500;

/// One poll of the four endpoints.
pub struct Poll {
    /// `/api/pool/stats`.
    pub stats: Value,
    /// `/api/pool/workers`.
    pub workers: Value,
    /// `/api/pool/shares`.
    pub shares: Value,
    /// `/api/pool/blocks`.
    pub blocks: Value,
}

/// Reads the four endpoints.
pub fn fetch(ep: &Endpoint) -> Result<Poll, String> {
    Ok(Poll {
        stats: ep.get_json("/api/pool/stats")?,
        workers: ep.get_json("/api/pool/workers")?,
        shares: ep.get_json("/api/pool/shares")?,
        blocks: ep.get_json("/api/pool/blocks")?,
    })
}

fn f64_of(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn u64_of(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// A string the stratum reports, at most 128 characters (miners choose
/// worker names and addresses).
fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").chars().take(128).collect()
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Go's `time.Time` JSON (RFC 3339, UTC `Z` or an offset) → unix
/// milliseconds; the zero time (`0001-01-01T00:00:00Z`) and anything
/// unparsable → 0.
pub fn go_time_ms(s: &str) -> u64 {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return 0;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) = (
        num(0..4),
        num(5..7),
        num(8..10),
        num(11..13),
        num(14..16),
        num(17..19),
    ) else {
        return 0;
    };
    if y < 1970 {
        return 0;
    }
    let mut rest = &s[19..];
    let mut ms = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        rest = &frac[digits.len()..];
        let padded = format!("{digits:0<3}");
        ms = padded[..3].parse().unwrap_or(0);
    }
    let offset_s = match rest.as_bytes().first() {
        Some(b'+') | Some(b'-') if rest.len() >= 6 => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let oh: i64 = rest.get(1..3).and_then(|v| v.parse().ok()).unwrap_or(0);
            let om: i64 = rest.get(4..6).and_then(|v| v.parse().ok()).unwrap_or(0);
            sign * (oh * 3600 + om * 60)
        }
        _ => 0,
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset_s;
    (secs * 1000 + ms).max(0) as u64
}

fn algo_block(v: &Value) -> StratumAlgo {
    StratumAlgo {
        hashrate: f64_of(&v["hashrate"]),
        workers: u64_of(&v["workers"]) as u32,
        shares_valid: u64_of(&v["sharesValid"]),
        ..Default::default()
    }
}

/// The network's figures for one algorithm (`quai_getMiningInfo`) applied
/// to this node's hashrate.
fn against_network(a: &mut StratumAlgo, net_hashrate: f64, share_time: f64) {
    if net_hashrate > 0.0 {
        a.network_share = (a.hashrate / net_hashrate).min(1.0);
        if share_time > 0.0 {
            a.expected_per_hour = a.network_share * 3600.0 / share_time;
        }
    }
}

/// Builds the stratum view from one poll. `mining` (the node's
/// `quai_getMiningInfo`) supplies the network side.
pub fn build(api: &str, p: &Poll, mining: Option<&Mining>) -> Stratum {
    let s = &p.stats;
    let mut st = Stratum {
        api: api.to_string(),
        online: true,
        uptime: f64_of(&s["uptime"]),
        workers_connected: u64_of(&s["workersConnected"]) as u32,
        workers_total: u64_of(&s["workersTotal"]) as u32,
        shares_valid: u64_of(&s["sharesValid"]),
        shares_stale: u64_of(&s["sharesStale"]),
        shares_invalid: u64_of(&s["sharesInvalid"]),
        workshares_found: u64_of(&s["blocksFound"]),
        kawpow: algo_block(&s["kawpow"]),
        sha: algo_block(&s["sha256"]),
        scrypt: algo_block(&s["scrypt"]),
        mined: s["mined"].as_object().map(|m| crate::state::StratumMined {
            prime: u64_of(&m["prime"]),
            region: u64_of(&m["region"]),
            zone: u64_of(&m["zone"]),
            workshares: u64_of(&m["workshares"]),
            workshares_paid: u64_of(&m["worksharesPaid"]),
        }),
        ..Default::default()
    };
    if let Some(m) = mining {
        against_network(&mut st.kawpow, m.kawpow.hashrate, m.kawpow.share_time);
        against_network(&mut st.sha, m.sha.hashrate, m.sha.share_time);
        against_network(&mut st.scrypt, m.scrypt.hashrate, m.scrypt.share_time);
    }
    let mut workers: Vec<StratumWorker> = p
        .workers
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["isConnected"].as_bool().unwrap_or(true))
        .map(|w| StratumWorker {
            address: str_of(&w["address"]),
            name: str_of(&w["workerName"]),
            algorithm: str_of(&w["algorithm"]),
            difficulty: f64_of(&w["difficulty"]),
            hashrate: f64_of(&w["hashrate"]),
            valid: u64_of(&w["sharesValid"]),
            stale: u64_of(&w["sharesStale"]),
            invalid: u64_of(&w["sharesInvalid"]),
            last_share_ms: go_time_ms(w["lastShareAt"].as_str().unwrap_or("")),
            connected_ms: go_time_ms(w["connectedAt"].as_str().unwrap_or("")),
        })
        .collect();
    workers.sort_by(|a, b| {
        b.hashrate
            .total_cmp(&a.hashrate)
            .then(b.valid.cmp(&a.valid))
            .then(a.address.cmp(&b.address))
            .then(a.name.cmp(&b.name))
    });
    st.miners = workers
        .iter()
        .map(|w| w.address.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>()
        .len() as u32;
    workers.truncate(WORKERS_KEPT);
    let mut by: BTreeMap<String, AddressPaid> = BTreeMap::new();
    for w in &workers {
        let e = by
            .entry(w.address.to_ascii_lowercase())
            .or_insert_with(|| AddressPaid {
                address: w.address.clone(),
                ..Default::default()
            });
        e.workers += 1;
        if !e.algorithms.contains(&w.algorithm) {
            e.algorithms.push(w.algorithm.clone());
        }
    }
    st.onchain.by_address = by.into_values().collect();
    st.workers = workers;
    let sh = &p.shares;
    st.luck = ShareLuck {
        shares: u64_of(&sh["totalShares"]),
        workshare_diff: f64_of(&sh["workshareDiff"]),
        average: f64_of(&sh["averageLuck"]),
        best: f64_of(&sh["bestShareLuck"]),
        expected_shares: f64_of(&sh["expectedShares"]),
    };
    st.found = p
        .blocks
        .as_array()
        .into_iter()
        .flatten()
        .take(FOUND_KEPT)
        .map(|b| FoundShare {
            height: u64_of(&b["height"]),
            hash: str_of(&b["hash"]).to_ascii_lowercase(),
            worker: str_of(&b["worker"]),
            algorithm: str_of(&b["algorithm"]),
            found_ms: go_time_ms(b["foundAt"].as_str().unwrap_or("")),
            status: "pending".into(),
        })
        .collect();
    st
}

fn norm_hash(h: &str) -> String {
    let h = h.trim_start_matches("0x").to_ascii_lowercase();
    format!("0x{h}")
}

/// Marks the recent canonical blocks paid to the stratum's miners, counts
/// them, and settles the status of each workshare the stratum handed to
/// the node: the canonical block at its height, a workshare in a canonical
/// block, still pending, or older than the window.
pub fn settle(st: &mut Stratum, blocks: &mut VecDeque<BlockInfo>) {
    let miners: HashMap<String, usize> = st
        .onchain
        .by_address
        .iter()
        .enumerate()
        .map(|(i, a)| (a.address.to_ascii_lowercase(), i))
        .collect();
    let mut oc = OnChain {
        window: blocks.len() as u32,
        by_address: std::mem::take(&mut st.onchain.by_address),
        ..Default::default()
    };
    for a in &mut oc.by_address {
        a.workshares = 0;
        a.blocks = 0;
    }
    let mut block_hashes: HashMap<String, u64> = HashMap::new();
    let mut ws_hashes: HashMap<String, u64> = HashMap::new();
    for b in blocks.iter_mut() {
        block_hashes.insert(norm_hash(&b.hash), b.number);
        for h in &b.ws_hashes {
            ws_hashes.insert(norm_hash(h), b.number);
        }
        b.ours = 0;
        b.ours_block = false;
        if let Some(&i) = miners.get(&b.coinbase.to_ascii_lowercase()) {
            b.ours_block = true;
            oc.blocks += 1;
            oc.by_address[i].blocks += 1;
        }
        for c in &b.ws_coinbases {
            if let Some(&i) = miners.get(&c.to_ascii_lowercase()) {
                b.ours += 1;
                oc.workshares += 1;
                oc.by_address[i].workshares += 1;
            }
        }
    }
    let oldest = blocks.front().map_or(u64::MAX, |b| b.number);
    for f in &mut st.found {
        let h = norm_hash(&f.hash);
        f.status = if block_hashes.contains_key(&h) {
            oc.found_blocks += 1;
            "block"
        } else if ws_hashes.contains_key(&h) {
            oc.found_included += 1;
            "included"
        } else if f.height < oldest {
            "unseen"
        } else {
            "pending"
        }
        .into();
    }
    st.onchain = oc;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    #[test]
    fn odd_offsets_do_not_panic() {
        assert_eq!(
            super::go_time_ms("2026-10-03T20:00:00+€:00"),
            super::go_time_ms("2026-10-03T20:00:00Z")
        );
        let _ = super::go_time_ms("2026-10-03T20:00:00-0é:0");
    }

    use serde_json::json;

    use super::*;
    use crate::state::Algo;

    #[test]
    fn a_crowded_stratum_is_bounded() {
        let mut p = live();
        let workers: Vec<Value> = (0..2000)
            .map(|i| {
                json!({"address": format!("0x{:040x}{}", i % 700, "€".repeat(200)),
                    "workerName": "w".repeat(10_000), "algorithm": "kawpow", "hashrate": i})
            })
            .collect();
        p.workers = Value::Array(workers);
        let st = build("x", &p, None);
        assert_eq!(st.workers.len(), WORKERS_KEPT);
        assert_eq!(st.miners, 700);
        assert!(st.onchain.by_address.len() <= WORKERS_KEPT);
        assert!(
            st.workers
                .iter()
                .all(|w| w.name.chars().count() <= 128 && w.address.chars().count() <= 128)
        );
        // The fastest are the ones kept.
        assert_eq!(st.workers[0].hashrate, 1999.0);
    }

    /// `/api/pool/*` from the soak node's stratum on 2026-10-03, one KawPoW
    /// GPU worker connected (public addresses only).
    fn live() -> Poll {
        Poll {
            stats: json!({"workersTotal":1,"workersConnected":1,"hashrate":0,"sharesValid":0,
                "sharesStale":0,"sharesInvalid":0,"blocksFound":0,"uptime":205.94120501,
                "startedAt":"2026-10-03T21:57:14.609206343Z","blockHeight":10428163,
                "sha256":{"hashrate":0,"networkDifficulty":0,"networkHashrate":0,"workers":0,"sharesValid":0},
                "scrypt":{"hashrate":0,"networkDifficulty":0,"networkHashrate":0,"workers":0,"sharesValid":0},
                "kawpow":{"hashrate":0,"networkDifficulty":0,"networkHashrate":0,"workers":1,"sharesValid":0}}),
            workers: json!([{"address":"0x00051234AbCdEf0123456789aBcDeF01234567Fe","workerName":"gpu0",
                "algorithm":"kawpow","connectedAt":"2026-10-03T21:57:14.613799338Z",
                "firstShareAt":"0001-01-01T00:00:00Z","firstShareDiff":0,
                "lastShareAt":"0001-01-01T00:00:00Z","lastShareDiff":0,"sharesValid":0,
                "sharesStale":0,"sharesInvalid":0,"difficulty":0,"hashrate":0,"isConnected":true}]),
            shares: json!({"shares":[],"workshareDiff":0,"averageLuck":0,"bestShareLuck":0,
                "totalShares":0,"blocksFound":0,"expectedShares":0}),
            blocks: json!([]),
        }
    }

    /// Several miners on all three algorithms, shares and handed-off
    /// workshares, in the API's shapes (`api.rs`).
    fn busy() -> Poll {
        Poll {
            stats: json!({"workersTotal":4,"workersConnected":3,"hashrate":1.0e15,"sharesValid":120,
                "sharesStale":3,"sharesInvalid":1,"blocksFound":2,"uptime":3600.0,
                "startedAt":"2026-10-03T20:00:00Z",
                "sha256":{"hashrate":1.0e15,"networkDifficulty":0,"networkHashrate":0,"workers":1,"sharesValid":80},
                "scrypt":{"hashrate":1.0e11,"networkDifficulty":0,"networkHashrate":0,"workers":1,"sharesValid":30},
                "kawpow":{"hashrate":3.0e7,"networkDifficulty":0,"networkHashrate":0,"workers":1,"sharesValid":10}}),
            workers: json!([
                {"address":"0x00aa","workerName":"gpu0","algorithm":"kawpow","connectedAt":"2026-10-03T20:00:00Z",
                 "lastShareAt":"2026-10-03T20:59:30.5Z","sharesValid":10,"sharesStale":0,"sharesInvalid":1,
                 "difficulty":0.2,"hashrate":3.0e7,"isConnected":true},
                {"address":"0x00AA","workerName":"s19","algorithm":"sha256","connectedAt":"2026-10-03T20:00:00Z",
                 "lastShareAt":"2026-10-03T20:59:59Z","sharesValid":80,"sharesStale":3,"sharesInvalid":0,
                 "difficulty":65536,"hashrate":1.0e15,"isConnected":true},
                {"address":"0x00bb","workerName":"l7","algorithm":"scrypt","connectedAt":"2026-10-03T20:10:00Z",
                 "lastShareAt":"0001-01-01T00:00:00Z","sharesValid":30,"sharesStale":0,"sharesInvalid":0,
                 "difficulty":512,"hashrate":1.0e11,"isConnected":true},
                {"address":"0x00cc","workerName":"old","algorithm":"kawpow","connectedAt":"2026-10-03T20:00:00Z",
                 "lastShareAt":"0001-01-01T00:00:00Z","sharesValid":0,"sharesStale":0,"sharesInvalid":0,
                 "difficulty":0,"hashrate":0,"isConnected":false}]),
            shares: json!({"shares":[],"workshareDiff":4500.0,"averageLuck":0.4,"bestShareLuck":112.5,
                "totalShares":120,"blocksFound":2,"expectedShares":9000.0}),
            blocks: json!([
                {"height":101,"hash":"0xB2","worker":"0x00aa.s19","algorithm":"sha256","difficulty":"5000.00",
                 "foundAt":"2026-10-03T20:50:00Z"},
                {"height":99,"hash":"0xa1","worker":"0x00bb.l7","algorithm":"scrypt","difficulty":"4600.00",
                 "foundAt":"2026-10-03T20:40:00Z"},
                {"height":90,"hash":"0xc3","worker":"0x00aa.gpu0","algorithm":"kawpow","difficulty":"4700.00",
                 "foundAt":"2026-10-03T20:30:00Z"},
                {"height":104,"hash":"0xd4","worker":"0x00aa.gpu0","algorithm":"kawpow","difficulty":"4800.00",
                 "foundAt":"2026-10-03T20:55:00Z"}]),
        }
    }

    fn mining() -> Mining {
        let a = |h: f64, t: f64| Algo {
            hashrate: h,
            difficulty: "0".into(),
            share_time: t,
        };
        Mining {
            kawpow: a(3.0e11, 5.0),
            sha: a(2.5e17, 1.2),
            scrypt: a(3.1e13, 1.1),
            ..Default::default()
        }
    }

    fn block(n: u64, hash: &str, coinbase: &str, ws: &[(&str, &str)]) -> BlockInfo {
        BlockInfo {
            number: n,
            hash: hash.into(),
            coinbase: coinbase.into(),
            workshares: ws.len() as u32,
            ws_coinbases: ws.iter().map(|w| w.0.to_string()).collect(),
            ws_hashes: ws.iter().map(|w| w.1.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn go_times() {
        assert_eq!(go_time_ms("0001-01-01T00:00:00Z"), 0);
        assert_eq!(go_time_ms(""), 0);
        assert_eq!(go_time_ms("1970-01-01T00:00:01Z"), 1000);
        assert_eq!(
            go_time_ms("2026-10-03T21:57:14.613799338Z"),
            1_791_064_634_613
        );
        assert_eq!(go_time_ms("2026-10-03T20:59:30.5Z"), 1_791_061_170_500);
        assert_eq!(
            go_time_ms("2026-10-03T16:57:14-05:00"),
            go_time_ms("2026-10-03T21:57:14Z")
        );
        assert_eq!(go_time_ms("2000-02-29T00:00:00Z"), 951_782_400_000);
    }

    /// rs-quai's `mined` tally is read when present; go-quai's stats (the
    /// `live()` fixture has none) leave it out.
    #[test]
    fn reads_the_mined_tally_when_reported() {
        assert_eq!(build("x", &live(), None).mined, None);
        let mut p = live();
        p.stats["mined"] = serde_json::json!({"prime": 0, "region": 1, "zone": 2, "workshares": 40, "worksharesPaid": 31});
        let m = build("x", &p, None).mined.unwrap();
        assert_eq!(
            (m.prime, m.region, m.zone, m.workshares, m.workshares_paid),
            (0, 1, 2, 40, 31)
        );
    }

    #[test]
    fn parses_the_live_stratum() {
        let st = build("http://127.0.0.1:3336", &live(), Some(&mining()));
        assert!(st.online);
        assert_eq!(
            (st.workers_connected, st.workers_total, st.miners),
            (1, 1, 1)
        );
        assert_eq!(st.kawpow.workers, 1);
        assert_eq!(st.workers.len(), 1);
        let w = &st.workers[0];
        assert_eq!(w.name, "gpu0");
        assert_eq!(w.algorithm, "kawpow");
        assert_eq!(w.last_share_ms, 0);
        assert_eq!(w.connected_ms, 1_791_064_634_613);
        assert!(st.found.is_empty());
        assert_eq!(st.onchain.by_address[0].workers, 1);
    }

    #[test]
    fn parses_many_miners_and_workers() {
        let st = build("x", &busy(), Some(&mining()));
        // The disconnected worker is dropped; two addresses (case-insensitive).
        assert_eq!(st.workers.len(), 3);
        assert_eq!(st.miners, 2);
        assert_eq!(st.workers[0].name, "s19", "busiest first");
        assert_eq!(
            (st.shares_valid, st.shares_stale, st.shares_invalid),
            (120, 3, 1)
        );
        assert_eq!(st.workshares_found, 2);
        assert_eq!(st.luck.workshare_diff, 4500.0);
        assert_eq!(st.found.len(), 4);
        assert_eq!(st.found[0].hash, "0xb2");
        let a = st
            .onchain
            .by_address
            .iter()
            .find(|a| a.address.eq_ignore_ascii_case("0x00aa"))
            .unwrap();
        assert_eq!(a.workers, 2);
        assert_eq!(a.algorithms, ["sha256", "kawpow"]);
        // 30 MH/s of 300 GH/s, one network share every 5 s: 0.072 an hour.
        assert!((st.kawpow.network_share - 1e-4).abs() < 1e-12);
        assert!((st.kawpow.expected_per_hour - 0.072).abs() < 1e-9);
        // No network figures: nothing expected.
        let bare = build("x", &busy(), None);
        assert_eq!(bare.kawpow.expected_per_hour, 0.0);
    }

    #[test]
    fn counts_what_the_chain_paid_and_settles_found_shares() {
        let mut st = build("x", &busy(), Some(&mining()));
        let mut blocks: VecDeque<BlockInfo> = [
            block(98, "0x98", "0x00dd", &[("0x00AA", "0xe1")]),
            block(99, "0x99", "0x00dd", &[]),
            block(
                100,
                "0x100",
                "0x00dd",
                &[("0x00bb", "0xA1"), ("0x00ee", "0xe2")],
            ),
            block(
                101,
                "0xb2",
                "0x00aa",
                &[("0x00aa", "0xe3"), ("0x00aa", "0xe4")],
            ),
            block(102, "0x102", "0x00dd", &[]),
        ]
        .into();
        settle(&mut st, &mut blocks);
        let oc = &st.onchain;
        assert_eq!(oc.window, 5);
        assert_eq!(oc.workshares, 4, "0x00AA once, 0x00bb once, 0x00aa twice");
        assert_eq!(oc.blocks, 1);
        assert_eq!((oc.found_blocks, oc.found_included), (1, 1));
        let st_of = |h: &str| {
            st.found
                .iter()
                .find(|f| f.hash == h)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(st_of("0xb2"), "block");
        assert_eq!(st_of("0xa1"), "included");
        assert_eq!(st_of("0xc3"), "unseen", "height 90 is before the window");
        assert_eq!(st_of("0xd4"), "pending");
        assert_eq!(blocks[0].ours, 1);
        assert_eq!(blocks[2].ours, 1, "0x00ee is not mining here");
        assert!(blocks[3].ours_block && blocks[3].ours == 2);
        assert!(!blocks[4].ours_block);
        let aa = oc
            .by_address
            .iter()
            .find(|a| a.address.eq_ignore_ascii_case("0x00aa"))
            .unwrap();
        assert_eq!((aa.workshares, aa.blocks), (3, 1));
        // Settling again (the next poll) does not double count.
        settle(&mut st, &mut blocks);
        assert_eq!(st.onchain.workshares, 4);
    }
}
