//! `--demo`: an invented node, for trying quai-dash without one and for
//! recordings. Blocks arrive about every 5 s with mainnet-like tier mix,
//! hashrates and peers; every view labels the data as demo.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::{
    AddressPaid, Algo, BLOCK_HISTORY, BlockInfo, Chains, Compare, FoundShare, Head, Mining, Peer,
    Peers, PendingShares, Place, ShareLuck, State, Stratum, StratumAlgo, StratumWorker, now_ms,
};

/// The demo stratum's workers: payout address, name, algorithm, hashes per
/// second, stratum difficulty.
const WORKERS: &[(&str, &str, &str, f64, f64)] = &[
    (
        "0x0042f6d1c9a3b2e7f05a44c1d8e2b9a7c6f30a11",
        "farm-a",
        "kawpow",
        1.21e9,
        2.0,
    ),
    (
        "0x0042f6d1c9a3b2e7f05a44c1d8e2b9a7c6f30a11",
        "farm-b",
        "kawpow",
        0.88e9,
        2.0,
    ),
    (
        "0x00771a5e0b3c9d24e6f8a1b2c3d4e5f60718293a",
        "s21-01",
        "sha256",
        2.31e14,
        65536.0,
    ),
    (
        "0x00771a5e0b3c9d24e6f8a1b2c3d4e5f60718293a",
        "s21-02",
        "sha256",
        2.27e14,
        65536.0,
    ),
    (
        "0x0029c4b7a1d0e9f8c7b6a5948372615049382716",
        "s19-xp",
        "sha256",
        1.38e14,
        32768.0,
    ),
    (
        "0x0013e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a291",
        "l9-01",
        "scrypt",
        1.72e10,
        1024.0,
    ),
    (
        "0x0013e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a291",
        "l9-02",
        "scrypt",
        1.65e10,
        1024.0,
    ),
    (
        "0x00c4a9e3f1b7d5c3a1e9f7d5b3a1c9e7f5d3b1a9",
        "gpu0",
        "kawpow",
        3.04e7,
        0.2,
    ),
];

const CITIES: &[(f64, f64, &str, &str)] = &[
    (40.71, -74.0, "New York", "US"),
    (37.77, -122.42, "San Francisco", "US"),
    (41.88, -87.63, "Chicago", "US"),
    (30.27, -97.74, "Austin", "US"),
    (47.61, -122.33, "Seattle", "US"),
    (43.65, -79.38, "Toronto", "CA"),
    (51.51, -0.13, "London", "GB"),
    (52.52, 13.4, "Berlin", "DE"),
    (50.11, 8.68, "Frankfurt", "DE"),
    (48.86, 2.35, "Paris", "FR"),
    (52.37, 4.9, "Amsterdam", "NL"),
    (59.33, 18.07, "Stockholm", "SE"),
    (60.17, 24.94, "Helsinki", "FI"),
    (47.37, 8.54, "Zurich", "CH"),
    (40.42, -3.7, "Madrid", "ES"),
    (52.23, 21.01, "Warsaw", "PL"),
    (35.68, 139.69, "Tokyo", "JP"),
    (37.57, 126.98, "Seoul", "KR"),
    (1.35, 103.82, "Singapore", "SG"),
    (22.32, 114.17, "Hong Kong", "HK"),
    (-33.87, 151.21, "Sydney", "AU"),
    (19.08, 72.88, "Mumbai", "IN"),
    (25.2, 55.27, "Dubai", "AE"),
    (-23.55, -46.63, "São Paulo", "BR"),
    (-34.6, -58.38, "Buenos Aires", "AR"),
    (19.43, -99.13, "Mexico City", "MX"),
    (-26.2, 28.05, "Johannesburg", "ZA"),
    (41.01, 28.98, "Istanbul", "TR"),
    (39.74, -104.99, "Denver", "US"),
    (53.35, -6.26, "Dublin", "IE"),
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn hash(&mut self) -> String {
        format!(
            "0x{:016x}{:016x}{:016x}{:016x}",
            self.next(),
            self.next(),
            self.next(),
            self.next()
        )
    }
}

struct Demo {
    rng: Rng,
    zone: u64,
    region: u64,
    prime: u64,
    next_block_ms: u64,
    /// Valid, stale and invalid shares per worker.
    shares: Vec<(u64, u64, u64)>,
    /// Workshares the demo stratum handed to the node, newest first.
    found: Vec<FoundShare>,
    started_ms: u64,
}

impl Demo {
    fn mine(&mut self, st: &mut State, t_ms: u64, quiet: bool) {
        self.zone += 1;
        let r = self.rng.f();
        let order: u8 = if r < 0.12 {
            0
        } else if r < 0.45 {
            1
        } else {
            2
        };
        if order <= 1 {
            self.region += 1;
        }
        if order == 0 {
            self.prime += 1;
        }
        let parent = st
            .blocks
            .back()
            .map_or_else(|| self.rng.hash(), |b| b.hash.clone());
        // Other miners' workshares, plus up to two of the demo stratum's
        // pending ones; now and then the block itself was found through
        // this node.
        let mut ws_coinbases: Vec<String> = Vec::new();
        let mut ws_hashes: Vec<String> = Vec::new();
        for _ in 0..(self.rng.f() * 10.0) as usize {
            ws_coinbases.push(format!("0x00{}", &self.rng.hash()[4..42]));
            ws_hashes.push(self.rng.hash());
        }
        let mut taken = 0;
        for i in 0..self.found.len() {
            if self.found[i].status == "pending" && taken < 2 && self.rng.f() < 0.7 {
                let f = &self.found[i];
                ws_coinbases.push(f.worker.split('.').next().unwrap_or("").to_string());
                ws_hashes.push(f.hash.clone());
                taken += 1;
            }
        }
        let hash = self.rng.hash();
        let coinbase = if !quiet && self.rng.f() < 0.06 {
            let w = WORKERS[(self.rng.next() % WORKERS.len() as u64) as usize];
            let worker = format!("{}.{}", w.0, w.1);
            st.push_event(
                t_ms,
                "mined",
                format!(
                    "Block {} mined through this node by {} ({})",
                    self.zone,
                    crate::collect::short_worker(&worker),
                    w.2
                ),
                Some(self.zone),
            );
            self.found.insert(
                0,
                FoundShare {
                    height: self.zone,
                    hash: hash.clone(),
                    worker,
                    algorithm: w.2.into(),
                    found_ms: t_ms,
                    status: "block".into(),
                },
            );
            w.0.to_string()
        } else {
            format!("0x00{}", &self.rng.hash()[4..42])
        };
        let b = BlockInfo {
            number: self.zone,
            hash,
            parent,
            timestamp: t_ms / 1000,
            seen_ms: t_ms,
            txs: (self.rng.f().powi(3) * 40.0) as u32,
            etxs: (self.rng.f() * 18.0) as u32,
            workshares: ws_hashes.len() as u32,
            gas_used: (self.rng.f().powi(2) * 18e6) as u64,
            gas_limit: 50_000_000,
            base_fee: format!(
                "{}",
                26_000_000_000_000u64 + self.rng.next() % 2_000_000_000_000
            ),
            order,
            difficulty: format!("{}", 1_038_000_000_000u64 + self.rng.next() % 4_000_000_000),
            coinbase,
            auxpow: true,
            prime_number: self.prime,
            region_number: self.region,
            exchange_rate: "13264669140000000000".into(),
            size: 3000 + self.rng.next() % 9000,
            ours: 0,
            ours_block: false,
            ws_coinbases,
            ws_hashes,
        };
        if !quiet {
            st.push_log(
                "INFO".into(),
                format!(
                    "{}  INFO rsq_chain::chaincore: Appended new block number={} hash={}… txs={} etxs={} workshares={}",
                    iso(t_ms),
                    b.number,
                    &b.hash[..18],
                    b.txs,
                    b.etxs,
                    b.workshares
                ),
            );
            match order {
                0 => st.push_event(
                    t_ms,
                    "prime",
                    format!("Prime block {} (zone {})", self.prime, b.number),
                    Some(b.number),
                ),
                1 => st.push_event(
                    t_ms,
                    "region",
                    format!("Region block {} (zone {})", self.region, b.number),
                    Some(b.number),
                ),
                _ => {}
            }
            if self.rng.f() < 0.08 {
                st.push_log("WARN".into(), format!("{}  WARN rsq_chain::indexer: ChainIndexer: Reorging the utxo indexer len=1", iso(t_ms)));
            }
        }
        st.chains = Chains {
            prime: Some(Head {
                number: self.prime,
                hash: String::new(),
                timestamp: b.timestamp,
            }),
            region: Some(Head {
                number: self.region,
                hash: String::new(),
                timestamp: b.timestamp,
            }),
            zone: Some(Head {
                number: b.number,
                hash: b.hash.clone(),
                timestamp: b.timestamp,
            }),
        };
        st.blocks.push_back(b);
        while st.blocks.len() > BLOCK_HISTORY {
            st.blocks.pop_front();
        }
    }

    fn tick(&mut self, st: &mut State) {
        let now = now_ms();
        while now >= self.next_block_ms {
            let t = self.next_block_ms;
            self.mine(st, t, false);
            self.next_block_ms += 1000 + (-(1.0 - self.rng.f()).ln() * 4200.0) as u64;
        }
        if self.rng.f() < 0.5 {
            let line = format!(
                "{}  INFO rsq_miner::worker: Workshare accepted hash={}… number={}",
                iso(now),
                &self.rng.hash()[..18],
                self.zone + 1
            );
            st.push_log("INFO".into(), line);
        }
        let mut wob = |x: f64, p: f64| x * (1.0 + (self.rng.f() - 0.5) * p);
        st.mining = Some(Mining {
            avg_block_time: wob(5.1, 0.05),
            blocks_analyzed: 178,
            kawpow: Algo {
                hashrate: wob(2.43e11, 0.06),
                difficulty: "1038275705430".into(),
                share_time: wob(5.1, 0.05),
            },
            sha: Algo {
                hashrate: wob(2.5e17, 0.08),
                difficulty: "220452383356020829".into(),
                share_time: wob(1.25, 0.1),
            },
            scrypt: Algo {
                hashrate: wob(7.08e12, 0.08),
                difficulty: "14544668614928".into(),
                share_time: wob(1.3, 0.1),
            },
            base_block_reward: "70.4023".into(),
            estimated_block_reward: "105.3384".into(),
            workshare_reward: "11.7042".into(),
            quai_supply: "1124728606".into(),
            avg_tx_fees: "17.4680".into(),
        });
        st.pending_shares = PendingShares {
            kawpow: (self.rng.next() % 3) as u32,
            sha: (self.rng.next() % 10) as u32,
            scrypt: (self.rng.next() % 8) as u32,
            progpow: 0,
        };
        self.stratum(st, now);
        if let Some(c) = st.compare.as_mut() {
            c.height = self.zone;
            c.compared = (c.compared + 1).min(64);
            c.matched = c.compared;
        }
        st.now_ms = now;
    }
}

impl Demo {
    /// The demo stratum: each worker submits a share about every 30 s, and
    /// about every 30 s one of them meets the workshare target and goes to
    /// the node.
    fn stratum(&mut self, st: &mut State, now: u64) {
        for i in 0..WORKERS.len() {
            if self.rng.f() < 1.0 / 30.0 {
                let r = self.rng.f();
                let sh = &mut self.shares[i];
                if r < 0.985 {
                    sh.0 += 1;
                } else if r < 0.995 {
                    sh.1 += 1;
                } else {
                    sh.2 += 1;
                }
            }
        }
        if self.rng.f() < 1.0 / 30.0 {
            let w = WORKERS[(self.rng.next() % WORKERS.len() as u64) as usize];
            let f = FoundShare {
                height: self.zone + 1,
                hash: self.rng.hash(),
                worker: format!("{}.{}", w.0, w.1),
                algorithm: w.2.into(),
                found_ms: now,
                status: "pending".into(),
            };
            st.push_event(
                now,
                "workshare",
                format!(
                    "Workshare from {} ({}) at zone {}",
                    crate::collect::short_worker(&f.worker),
                    f.algorithm,
                    f.height
                ),
                Some(f.height),
            );
            self.found.insert(0, f);
        }
        self.found.truncate(32);
        let mut workers: Vec<StratumWorker> = Vec::new();
        for (i, w) in WORKERS.iter().enumerate() {
            let jitter = 1.0 + (self.rng.f() - 0.5) * 0.08;
            workers.push(StratumWorker {
                address: w.0.into(),
                name: w.1.into(),
                algorithm: w.2.into(),
                difficulty: w.4,
                hashrate: w.3 * jitter,
                valid: self.shares[i].0,
                stale: self.shares[i].1,
                invalid: self.shares[i].2,
                last_share_ms: now - (i as u64 * 7_300) % 29_000,
                connected_ms: self.started_ms - 3_600_000 * (i as u64 % 3 + 1),
            });
        }
        workers.sort_by(|a, b| b.hashrate.total_cmp(&a.hashrate));
        let algo = |name: &str, net: Option<&Algo>| {
            let ws: Vec<&StratumWorker> = workers.iter().filter(|w| w.algorithm == name).collect();
            let hashrate: f64 = ws.iter().map(|w| w.hashrate).sum();
            let mut a = StratumAlgo {
                hashrate,
                workers: ws.len() as u32,
                shares_valid: ws.iter().map(|w| w.valid).sum(),
                ..Default::default()
            };
            if let Some(n) = net.filter(|n| n.hashrate > 0.0) {
                a.network_share = hashrate / n.hashrate;
                a.expected_per_hour = a.network_share * 3600.0 / n.share_time.max(0.1);
            }
            a
        };
        let m = st.mining.clone();
        let mut by: std::collections::BTreeMap<String, AddressPaid> = Default::default();
        for w in &workers {
            let e = by.entry(w.address.clone()).or_insert_with(|| AddressPaid {
                address: w.address.clone(),
                ..Default::default()
            });
            e.workers += 1;
            if !e.algorithms.contains(&w.algorithm) {
                e.algorithms.push(w.algorithm.clone());
            }
        }
        let mut s = Stratum {
            api: "demo".into(),
            online: true,
            uptime: (now - self.started_ms) as f64 / 1000.0 + 3600.0 * 3.0,
            workers_connected: workers.len() as u32,
            workers_total: workers.len() as u32 + 2,
            miners: by.len() as u32,
            shares_valid: self.shares.iter().map(|s| s.0).sum(),
            shares_stale: self.shares.iter().map(|s| s.1).sum(),
            shares_invalid: self.shares.iter().map(|s| s.2).sum(),
            workshares_found: 214 + self.found.len() as u64,
            kawpow: algo("kawpow", m.as_ref().map(|m| &m.kawpow)),
            sha: algo("sha256", m.as_ref().map(|m| &m.sha)),
            scrypt: algo("scrypt", m.as_ref().map(|m| &m.scrypt)),
            luck: ShareLuck {
                shares: self.shares.iter().map(|s| s.0).sum::<u64>().min(500),
                workshare_diff: 4523.7,
                average: 0.41,
                best: 131.6,
                expected_shares: 11_034.0,
            },
            found: self.found.clone(),
            workers,
            ..Default::default()
        };
        s.onchain.by_address = by.into_values().collect();
        crate::stratum::settle(&mut s, &mut st.blocks);
        // The node's own tally (rs-quai): a region block from before the
        // page opened, plus whatever the demo finds.
        let status = |k: &str| s.found.iter().filter(|f| f.status == k).count() as u64;
        s.mined = Some(crate::state::StratumMined {
            prime: 0,
            region: 1,
            zone: 2 + status("block"),
            workshares: s.workshares_found,
            workshares_paid: 176 + status("included"),
        });
        for f in &mut self.found {
            if let Some(g) = s.found.iter().find(|g| g.hash == f.hash) {
                f.status = g.status.clone();
            }
        }
        st.stratum = Some(s);
    }
}

fn iso(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = ((s / 3600) % 24, (s / 60) % 60, s % 60);
    format!(
        "2026-10-01T{h:02}:{m:02}:{sec:02}.{:06}Z",
        (ms % 1000) * 1000
    )
}

/// Runs the invented node forever.
pub fn run(state: Arc<Mutex<State>>) {
    let now = now_ms();
    let mut d = Demo {
        rng: Rng(0x9E37_79B9_7F4A_7C15 ^ now),
        zone: 10_390_410,
        region: 5_579_751,
        prime: 2_297_891,
        next_block_ms: now + 2500,
        shares: (0..WORKERS.len() as u64)
            .map(|i| (300 + (i * 97) % 400, (i * 3) % 7, i % 2))
            .collect(),
        found: Vec::new(),
        started_ms: now,
    };
    if let Ok(mut st) = state.lock() {
        st.node.label = "DEMO NODE".into();
        st.node.rpc = "demo".into();
        st.node.kind = "rs-quai".into();
        st.node.kind_how = "demo".into();
        st.node.location = "Cyprus-1".into();
        st.node.chain_id = Some(9);
        st.node.online = true;
        st.node.log_file = Some("demo".into());
        // A few workshares from before the dashboard opened, so the first
        // screen already shows some included on chain (newest first).
        for i in (0..7u64).rev() {
            let w = WORKERS[(i as usize * 3) % WORKERS.len()];
            d.found.push(FoundShare {
                height: d.zone - 30 + i * 4,
                hash: d.rng.hash(),
                worker: format!("{}.{}", w.0, w.1),
                algorithm: w.2.into(),
                found_ms: now - (200 - i * 25) * 1000,
                status: "pending".into(),
            });
        }
        for i in 0..40u64 {
            d.mine(&mut st, now - (40 - i) * 5000, true);
        }
        for i in 0..20 {
            st.push_log(
                "INFO".into(),
                format!(
                    "{}  INFO rsq_node::node: network peers={}",
                    iso(now),
                    80 + i % 5
                ),
            );
        }
        let mut peers = Vec::new();
        for i in 0..64usize {
            let (lat, lon, city, cc) = CITIES[i % CITIES.len()];
            peers.push(Peer {
                ip: format!("203.0.{}.{}", (i * 37) % 250, (i * 91) % 250),
                port: 4002,
                dir: if i % 7 == 0 {
                    "out".into()
                } else {
                    "in".into()
                },
                place: Some(Place {
                    lat: lat + (d.rng.f() - 0.5) * 3.0,
                    lon: lon + (d.rng.f() - 0.5) * 3.0,
                    city: city.into(),
                    country: cc.into(),
                }),
                since_ms: now,
            });
        }
        st.peers = Peers {
            count: 86,
            inbound: 76,
            outbound: 10,
            list: peers,
            geo: "db".into(),
            note: None,
            here: Some(Place {
                lat: 39.1,
                lon: -94.6,
                city: String::new(),
                country: String::new(),
            }),
        };
        st.compare = Some(Compare {
            label: "GO-QUAI".into(),
            rpc: "demo".into(),
            online: true,
            ..Default::default()
        });
    }
    loop {
        if let Ok(mut st) = state.lock() {
            d.tick(&mut st);
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}
