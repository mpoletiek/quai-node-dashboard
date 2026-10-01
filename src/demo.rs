//! `--demo`: an invented node, for trying quai-dash without one and for
//! recordings. Blocks arrive about every 5 s with mainnet-like tier mix,
//! hashrates and peers; every view labels the data as demo.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::state::{
    Algo, BLOCK_HISTORY, BlockInfo, Chains, Compare, Head, Mining, PendingShares, Peer, Peers, Place, State, now_ms,
};

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
        format!("0x{:016x}{:016x}{:016x}{:016x}", self.next(), self.next(), self.next(), self.next())
    }
}

struct Demo {
    rng: Rng,
    zone: u64,
    region: u64,
    prime: u64,
    next_block_ms: u64,
}

impl Demo {
    fn mine(&mut self, st: &mut State, t_ms: u64, quiet: bool) {
        self.zone += 1;
        let r = self.rng.f();
        let order: u8 = if r < 0.12 { 0 } else if r < 0.45 { 1 } else { 2 };
        if order <= 1 {
            self.region += 1;
        }
        if order == 0 {
            self.prime += 1;
        }
        let parent = st.blocks.back().map_or_else(|| self.rng.hash(), |b| b.hash.clone());
        let b = BlockInfo {
            number: self.zone,
            hash: self.rng.hash(),
            parent,
            timestamp: t_ms / 1000,
            seen_ms: t_ms,
            txs: (self.rng.f().powi(3) * 40.0) as u32,
            etxs: (self.rng.f() * 18.0) as u32,
            workshares: (self.rng.f() * 14.0) as u32,
            gas_used: (self.rng.f().powi(2) * 18e6) as u64,
            gas_limit: 50_000_000,
            base_fee: format!("{}", 26_000_000_000_000u64 + self.rng.next() % 2_000_000_000_000),
            order,
            difficulty: format!("{}", 1_038_000_000_000u64 + self.rng.next() % 4_000_000_000),
            coinbase: format!("0x00{}", &self.rng.hash()[4..42]),
            auxpow: true,
            prime_number: self.prime,
            region_number: self.region,
            exchange_rate: "13264669140000000000".into(),
            size: 3000 + self.rng.next() % 9000,
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
                0 => st.push_event(t_ms, "prime", format!("Prime block {} (zone {})", self.prime, b.number), Some(b.number)),
                1 => st.push_event(t_ms, "region", format!("Region block {} (zone {})", self.region, b.number), Some(b.number)),
                _ => {}
            }
            if self.rng.f() < 0.08 {
                st.push_log("WARN".into(), format!("{}  WARN rsq_chain::indexer: ChainIndexer: Reorging the utxo indexer len=1", iso(t_ms)));
            }
        }
        st.chains = Chains {
            prime: Some(Head { number: self.prime, hash: String::new(), timestamp: b.timestamp }),
            region: Some(Head { number: self.region, hash: String::new(), timestamp: b.timestamp }),
            zone: Some(Head { number: b.number, hash: b.hash.clone(), timestamp: b.timestamp }),
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
            kawpow: Algo { hashrate: wob(2.43e11, 0.06), difficulty: "1038275705430".into(), share_time: wob(5.1, 0.05) },
            sha: Algo { hashrate: wob(2.5e17, 0.08), difficulty: "220452383356020829".into(), share_time: wob(1.25, 0.1) },
            scrypt: Algo { hashrate: wob(7.08e12, 0.08), difficulty: "14544668614928".into(), share_time: wob(1.3, 0.1) },
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
        if let Some(c) = st.compare.as_mut() {
            c.height = self.zone;
            c.compared = (c.compared + 1).min(64);
            c.matched = c.compared;
        }
        st.now_ms = now;
    }
}

fn iso(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = ((s / 3600) % 24, (s / 60) % 60, s % 60);
    format!("2026-10-01T{h:02}:{m:02}:{sec:02}.{:06}Z", (ms % 1000) * 1000)
}

/// Runs the invented node forever.
pub fn run(state: Arc<Mutex<State>>) {
    let now = now_ms();
    let mut d = Demo { rng: Rng(0x9E37_79B9_7F4A_7C15 ^ now), zone: 10_390_410, region: 5_579_751, prime: 2_297_891, next_block_ms: now + 2500 };
    if let Ok(mut st) = state.lock() {
        st.node.label = "DEMO NODE".into();
        st.node.rpc = "demo".into();
        st.node.location = "Cyprus-1".into();
        st.node.chain_id = Some(9);
        st.node.online = true;
        st.node.log_file = Some("demo".into());
        for i in 0..40u64 {
            d.mine(&mut st, now - (40 - i) * 5000, true);
        }
        for i in 0..20 {
            st.push_log("INFO".into(), format!("{}  INFO rsq_node::node: network peers={}", iso(now), 80 + i % 5));
        }
        let mut peers = Vec::new();
        for i in 0..64usize {
            let (lat, lon, city, cc) = CITIES[i % CITIES.len()];
            peers.push(Peer {
                ip: format!("203.0.{}.{}", (i * 37) % 250, (i * 91) % 250),
                port: 4002,
                dir: if i % 7 == 0 { "out".into() } else { "in".into() },
                place: Some(Place { lat: lat + (d.rng.f() - 0.5) * 3.0, lon: lon + (d.rng.f() - 0.5) * 3.0, city: city.into(), country: cc.into() }),
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
            here: Some(Place { lat: 39.1, lon: -94.6, city: String::new(), country: String::new() }),
        };
        st.compare = Some(Compare { label: "GO-QUAI".into(), rpc: "demo".into(), online: true, ..Default::default() });
    }
    loop {
        if let Ok(mut st) = state.lock() {
            d.tick(&mut st);
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}
