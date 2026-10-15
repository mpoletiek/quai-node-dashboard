//! Whether the node is still syncing, and how fast it is catching up.
//!
//! quai-dash knows only its own node, not the network's height, so "behind"
//! comes from the age of the zone head: a node whose newest block is older
//! than 10,000 blocks' worth of the 5 s target block time (about 14 hours)
//! is syncing. It counts as caught up again once that age is under 5,000
//! blocks' worth; the gap between the two keeps the dashboards from
//! flapping between views. Block counts behind are the same estimate: the
//! head's age over 5 s.
//!
//! Speed and the ETA come from the last minute of samples (one a second,
//! from the collector). Integers throughout: the crate forbids float
//! arithmetic.

use std::collections::VecDeque;

use serde::Serialize;

/// The zone chain's target block time, in seconds.
pub const BLOCK_S: u64 = 5;
/// Behind by more than this many blocks' worth of time: syncing.
pub const ENTER_BLOCKS: u64 = 10_000;
/// Syncing until behind by less than this many blocks' worth of time.
pub const LEAVE_BLOCKS: u64 = 5_000;
/// Samples older than this are dropped.
const WINDOW_MS: u64 = 60_000;
/// Speed and ETA need samples spanning at least this long.
const MIN_SPAN_MS: u64 = 15_000;

/// The sync picture shared by the web and terminal dashboards.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Sync {
    /// Far enough behind to show the syncing view.
    pub syncing: bool,
    /// Age of the zone head, in seconds.
    pub behind_s: u64,
    /// `behind_s` over the target block time: an estimate.
    pub approx_blocks_behind: u64,
    /// Zone blocks appended per minute over the last minute.
    pub blocks_per_min: Option<u64>,
    /// Seconds until caught up at the current pace; `None` until there is
    /// enough data, or while the node isn't gaining on the network.
    pub eta_s: Option<u64>,
    /// How much of the chain's history the node has, in basis points
    /// (10,000 = all), once the chain's start is known.
    pub progress_bp: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    ms: u64,
    number: u64,
    ts: u64,
}

/// Turns the collector's view of the zone head into a [`Sync`], keeping
/// the samples and the syncing state between calls.
#[derive(Debug, Default)]
pub struct Tracker {
    samples: VecDeque<Sample>,
    syncing: bool,
}

impl Tracker {
    /// The sync picture at `now_ms`, given the zone head (number, unix
    /// timestamp) and the chain's start (block 1's timestamp).
    pub fn update(&mut self, now_ms: u64, head: Option<(u64, u64)>, start_ts: Option<u64>) -> Sync {
        let Some((number, ts)) = head else {
            return Sync {
                syncing: self.syncing,
                ..Sync::default()
            };
        };
        let now_s = now_ms / 1000;
        let behind_s = now_s.saturating_sub(ts);
        if !self.syncing && behind_s > ENTER_BLOCKS * BLOCK_S {
            self.syncing = true;
        } else if self.syncing && behind_s < LEAVE_BLOCKS * BLOCK_S {
            self.syncing = false;
        }

        // A lower head than before: the node resynced, or another node
        // answers now. Earlier samples say nothing about this one.
        if self.samples.back().is_some_and(|s| number < s.number) {
            self.samples.clear();
        }
        self.samples.push_back(Sample {
            ms: now_ms,
            number,
            ts,
        });
        while self
            .samples
            .front()
            .is_some_and(|s| now_ms.saturating_sub(s.ms) > WINDOW_MS)
        {
            self.samples.pop_front();
        }

        let (mut blocks_per_min, mut eta_s) = (None, None);
        if let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) {
            let span_ms = last.ms.saturating_sub(first.ms);
            if span_ms >= MIN_SPAN_MS {
                blocks_per_min = Some(last.number.saturating_sub(first.number) * 60_000 / span_ms);
                // The gap closes by the chain time synced less the wall
                // time passed: zero or less means it isn't closing.
                let chain_ms = last.ts.saturating_sub(first.ts) * 1000;
                if chain_ms > span_ms {
                    eta_s = Some(behind_s * span_ms / (chain_ms - span_ms));
                }
            }
        }

        let progress_bp = start_ts.filter(|&s| now_s > s).map(|s| {
            let done = ts.saturating_sub(s) * 10_000 / (now_s - s);
            done.min(10_000) as u32
        });

        Sync {
            syncing: self.syncing,
            behind_s,
            approx_blocks_behind: behind_s / BLOCK_S,
            blocks_per_min,
            eta_s,
            progress_bp,
        }
    }
}

/// A unix timestamp as a UTC date, `2025-02-04`.
pub fn utc_date(ts: u64) -> String {
    // Days to civil date (Howard Hinnant's algorithm), in integers.
    let z = (ts / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A span of seconds as its two largest units: `617d 15h`, `5h 12m`,
/// `16m 40s`, `9s`.
pub fn span(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Blocks a minute as blocks a second, one decimal: `100.0 blocks/s`.
pub fn per_second(blocks_per_min: u64) -> String {
    let tenths = blocks_per_min * 10 / 60;
    format!("{}.{} blocks/s", tenths / 10, tenths % 10)
}

/// Basis points as a percentage, two decimals: `62.31%`.
pub fn percent(bp: u32) -> String {
    format!("{}.{:02}%", bp / 100, bp % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(utc_date(START), "2025-02-04");
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(span(617 * 86_400 + 15 * 3600 + 59), "617d 15h");
        assert_eq!(span(5 * 3600 + 12 * 60), "5h 12m");
        assert_eq!(span(1000), "16m 40s");
        assert_eq!(span(9), "9s");
        assert_eq!(per_second(6_000), "100.0 blocks/s");
        assert_eq!(per_second(25), "0.4 blocks/s");
        assert_eq!(percent(6_231), "62.31%");
        assert_eq!(percent(5), "0.05%");
    }

    const NOW: u64 = 1_792_000_000; // unix seconds
    const START: u64 = 1_738_707_316; // mainnet Cyprus-1 block 1

    #[test]
    fn enters_and_leaves_with_a_gap() {
        let mut t = Tracker::default();
        let at = |t: &mut Tracker, blocks_behind: u64| {
            t.update(NOW * 1000, Some((1, NOW - blocks_behind * BLOCK_S)), None)
                .syncing
        };
        assert!(!at(&mut t, 9_000), "9k behind, not yet syncing");
        assert!(at(&mut t, 10_001), "over 10k: syncing");
        assert!(at(&mut t, 7_000), "still syncing between 5k and 10k");
        assert!(!at(&mut t, 4_999), "under 5k: caught up");
        assert!(!at(&mut t, 7_000), "and stays so until over 10k again");
    }

    #[test]
    fn speed_eta_and_progress() {
        let mut t = Tracker::default();
        // A node a year behind syncing 100 blocks (500 s of chain) a second.
        let ts0 = NOW - 365 * 86_400;
        let first = t.update(NOW * 1000, Some((1_000, ts0)), Some(START));
        assert_eq!(first.blocks_per_min, None, "one sample is no speed");
        assert_eq!(first.eta_s, None);
        let mut s = first;
        for i in 1..=30 {
            s = t.update(
                (NOW + i) * 1000,
                Some((1_000 + 100 * i, ts0 + 500 * i)),
                Some(START),
            );
        }
        assert!(s.syncing);
        assert_eq!(s.blocks_per_min, Some(6_000));
        // Behind 365 days less 30 × 499 s; closing at 499 s a second.
        assert_eq!(s.behind_s, 365 * 86_400 - 30 * 499);
        assert_eq!(s.eta_s, Some(s.behind_s / 499));
        assert_eq!(s.approx_blocks_behind, s.behind_s / BLOCK_S);
        let want = (ts0 + 15_000 - START) * 10_000 / (NOW + 30 - START);
        assert_eq!(s.progress_bp, Some(want as u32));
    }

    #[test]
    fn no_eta_while_falling_behind_and_reset_on_resync() {
        let mut t = Tracker::default();
        let ts0 = NOW - 100_000;
        let mut s = t.update(NOW * 1000, Some((10, ts0)), None);
        for i in 1..=20 {
            // A block every 10 s of wall time: slower than the chain.
            s = t.update((NOW + 10 * i) * 1000, Some((10 + i, ts0 + 5 * i)), None);
        }
        assert_eq!(s.blocks_per_min, Some(6));
        assert_eq!(s.eta_s, None, "not gaining: no ETA");
        assert_eq!(s.progress_bp, None, "start unknown");
        // The node starts over at a lower block: old samples are dropped.
        s = t.update((NOW + 201) * 1000, Some((3, ts0)), None);
        assert_eq!(s.blocks_per_min, None);
    }

    #[test]
    fn no_head_keeps_the_state() {
        let mut t = Tracker::default();
        assert!(t.update(NOW * 1000, Some((1, START)), None).syncing);
        let s = t.update(NOW * 1000, None, None);
        assert!(s.syncing);
        assert_eq!(s.behind_s, 0);
    }
}
