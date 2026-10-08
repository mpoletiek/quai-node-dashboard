//! Peer geolocation: a local MaxMind GeoLite2/GeoIP2 City database,
//! ip-api.com's batch endpoint (which sees the peers' IP addresses; only
//! when asked for), or off.
//!
//! Online lookups run on their own thread: the collector hands it new
//! addresses and picks up answers without waiting, so a slow, blocked or
//! hostile ip-api.com never holds up the dashboard. A failed request is
//! retried after a minute, then two, up to 30 minutes.
//!
//! Online lookups stay inside ip-api's free tier: one batch of at most 100
//! addresses per request, at most [`ONLINE_PER_MINUTE`] requests a minute
//! (the batch endpoint allows 15; the single one 45), a pause when the
//! service says the window is spent (`X-Rl: 0`, `X-Ttl`) or answers 429,
//! and every answer cached: an address is never asked about twice.
//! Private and reserved addresses are never sent.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::rpc::Endpoint;
use crate::state::Place;

/// ip-api.com's batch endpoint.
pub const ONLINE_URL: &str = "http://ip-api.com/batch";
/// Requests a minute to the batch endpoint (its free-tier limit is 15).
pub const ONLINE_PER_MINUTE: usize = 15;
/// Addresses per batch request (ip-api's maximum).
const BATCH: usize = 100;
/// First wait after a failed request; it doubles up to `MAX_BACKOFF`.
const BACKOFF: Duration = Duration::from_secs(60);
/// Longest wait between failed requests.
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// Addresses waiting for an online answer; more are given up on.
const QUEUE_MAX: usize = 5000;
/// Addresses remembered; past this the cache starts over.
const CACHE_MAX: usize = 50_000;

/// Where the database is looked for when none is given.
pub fn db_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/GeoIP"),
        PathBuf::from("/var/lib/GeoIP"),
    ];
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d).join("GeoIP"));
    }
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        dirs.push(PathBuf::from(h).join(".local/share/GeoIP"));
    }
    dirs.dedup();
    dirs
}

/// The first City database in `dirs`: `GeoLite2-City.mmdb` or
/// `GeoIP2-City.mmdb` first, then any other `*City*.mmdb`.
pub fn find_db(dirs: &[PathBuf]) -> Option<PathBuf> {
    for d in dirs {
        for name in ["GeoLite2-City.mmdb", "GeoIP2-City.mmdb"] {
            let p = d.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        let mut others: Vec<PathBuf> = std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "mmdb")
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().contains("City"))
            })
            .collect();
        others.sort();
        if let Some(p) = others.into_iter().next() {
            return Some(p);
        }
    }
    None
}

/// A sliding-window request limit with an extra pause the server can ask
/// for. Times are passed in, so tests need no clock.
#[derive(Debug)]
pub struct RateLimiter {
    max: usize,
    window: Duration,
    sent: VecDeque<Instant>,
    paused_until: Option<Instant>,
}

impl RateLimiter {
    /// At most `max` requests in any `window`.
    pub fn new(max: usize, window: Duration) -> RateLimiter {
        RateLimiter {
            max,
            window,
            sent: VecDeque::new(),
            paused_until: None,
        }
    }

    /// Takes a slot for a request at `now`, or says no.
    pub fn try_acquire(&mut self, now: Instant) -> bool {
        if self.paused_until.is_some_and(|t| now < t) {
            return false;
        }
        while self
            .sent
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= self.window)
        {
            self.sent.pop_front();
        }
        if self.sent.len() >= self.max {
            return false;
        }
        self.sent.push_back(now);
        true
    }

    /// No requests before `until`.
    pub fn pause_until(&mut self, until: Instant) {
        self.paused_until = Some(self.paused_until.map_or(until, |t| t.max(until)));
    }
}

/// Whether an address is worth looking up (public unicast).
pub fn routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_documentation()
                || v.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || s[0] == 0x2001 && s[1] == 0x0db8)
        }
    }
}

/// IP geolocation: a local MaxMind database, ip-api.com, or off.
pub enum Geo {
    /// Disabled.
    Off,
    /// GeoLite2/GeoIP2 City database.
    Db(maxminddb::Reader<Vec<u8>>),
    /// ip-api.com batch lookups (sends peer IPs to ip-api.com).
    Online(Online),
}

/// The collector's end of the online lookup thread.
pub struct Online {
    ask: Sender<IpAddr>,
    answers: Receiver<(IpAddr, Option<Place>)>,
    /// Asked and not yet answered.
    asked: HashSet<IpAddr>,
}

impl Online {
    /// Starts the lookup thread.
    fn spawn(ep: Endpoint, limit: RateLimiter, backoff: Duration) -> Online {
        let (ask, asks) = channel();
        let (tell, answers) = channel();
        std::thread::spawn(move || lookup_thread(ep, limit, backoff, asks, tell));
        Online {
            ask,
            answers,
            asked: HashSet::new(),
        }
    }
}

/// Answers addresses from `asks` in batches, within the rate limit,
/// backing off after failures, until the collector goes away.
fn lookup_thread(
    ep: Endpoint,
    mut limit: RateLimiter,
    base: Duration,
    asks: Receiver<IpAddr>,
    tell: Sender<(IpAddr, Option<Place>)>,
) {
    let mut queue: VecDeque<IpAddr> = VecDeque::new();
    let mut retry_at = Instant::now();
    let mut backoff = base;
    loop {
        if queue.is_empty() {
            match asks.recv() {
                Ok(ip) => queue.push_back(ip),
                Err(_) => return,
            }
        }
        while let Ok(ip) = asks.try_recv() {
            if queue.len() < QUEUE_MAX {
                queue.push_back(ip);
            } else if tell.send((ip, None)).is_err() {
                return;
            }
        }
        let now = Instant::now();
        let wait = retry_at.saturating_duration_since(now);
        if !wait.is_zero() || !limit.try_acquire(now) {
            std::thread::sleep(wait.clamp(Duration::from_millis(10), Duration::from_secs(1)));
            continue;
        }
        let chunk: Vec<IpAddr> = queue.iter().take(BATCH).copied().collect();
        match online_batch(&ep, &mut limit, &chunk) {
            Ok(rows) => {
                backoff = base;
                for row in rows {
                    queue.pop_front();
                    if tell.send(row).is_err() {
                        return;
                    }
                }
            }
            Err(_) => {
                retry_at = Instant::now() + backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

impl Geo {
    /// ip-api.com with the free-tier limit.
    pub fn online() -> Result<Geo, String> {
        Ok(Geo::Online(Online::spawn(
            Endpoint::new(ONLINE_URL, Duration::from_secs(6))?,
            RateLimiter::new(ONLINE_PER_MINUTE, Duration::from_secs(60)),
            BACKOFF,
        )))
    }

    /// `off`, `db` or `online`.
    pub fn kind(&self) -> &'static str {
        match self {
            Geo::Off => "off",
            Geo::Db(_) => "db",
            Geo::Online(..) => "online",
        }
    }

    /// Looks up `ips` not in `cache`, filling it (failures cache as `None`).
    /// Online, it never waits: new addresses go to the lookup thread, and
    /// answers that have arrived since the last call are filed.
    pub fn resolve(&mut self, ips: &[IpAddr], cache: &mut HashMap<IpAddr, Option<Place>>) {
        if cache.len() > CACHE_MAX {
            cache.clear();
        }
        if let Geo::Online(o) = self {
            while let Ok((ip, place)) = o.answers.try_recv() {
                o.asked.remove(&ip);
                cache.insert(ip, place);
            }
        }
        let mut todo: Vec<IpAddr> = Vec::new();
        for ip in ips {
            if cache.contains_key(ip) || todo.contains(ip) {
                continue;
            }
            if routable(*ip) {
                todo.push(*ip);
            } else {
                cache.insert(*ip, None);
            }
        }
        if todo.is_empty() {
            return;
        }
        match self {
            Geo::Off => {}
            Geo::Db(r) => {
                for ip in todo {
                    cache.insert(ip, lookup_db(r, ip));
                }
            }
            Geo::Online(o) => {
                for ip in todo {
                    if o.asked.insert(ip) && o.ask.send(ip).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// One batch request; the places of the addresses it answers, in order.
fn online_batch(
    ep: &Endpoint,
    limit: &mut RateLimiter,
    chunk: &[IpAddr],
) -> Result<Vec<(IpAddr, Option<Place>)>, String> {
    let body = Value::Array(
        chunk
            .iter()
            .map(|ip| json!({"query": ip.to_string(), "fields": "status,lat,lon,city,countryCode,query"}))
            .collect(),
    )
    .to_string();
    let reply = ep.post_reply(&body)?;
    // X-Rl: requests left in this window; X-Ttl: seconds until it resets.
    let ttl = reply
        .header("X-Ttl")
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(60);
    let spent = reply.header("X-Rl").and_then(|r| r.parse::<u64>().ok()) == Some(0);
    if spent || reply.code == 429 {
        limit.pause_until(Instant::now() + Duration::from_secs(ttl.clamp(1, 120)));
    }
    if reply.code != 200 {
        return Err(format!("HTTP {}", reply.code));
    }
    let Ok(Value::Array(rows)) = serde_json::from_slice::<Value>(&reply.body) else {
        return Err("ip-api: unexpected answer".into());
    };
    Ok(chunk
        .iter()
        .zip(rows)
        .map(|(ip, row)| (*ip, place_of(&row)))
        .collect())
}

/// One row of an ip-api batch answer.
fn place_of(row: &Value) -> Option<Place> {
    (row["status"] == "success").then(|| Place {
        lat: row["lat"].as_f64().unwrap_or(0.0),
        lon: row["lon"].as_f64().unwrap_or(0.0),
        city: row["city"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(64)
            .collect(),
        country: row["countryCode"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(8)
            .collect(),
    })
}

fn lookup_db(r: &maxminddb::Reader<Vec<u8>>, ip: IpAddr) -> Option<Place> {
    let res = r.lookup(ip).ok()?;
    let city: maxminddb::geoip2::City = res.decode().ok()??;
    let loc = city.location;
    Some(Place {
        lat: loc.latitude?,
        lon: loc.longitude?,
        city: city
            .city
            .names
            .english
            .map(str::to_string)
            .unwrap_or_default(),
        country: city
            .country
            .iso_code
            .map(str::to_string)
            .unwrap_or_default(),
    })
}

/// Opens a MaxMind database.
pub fn open_db(path: &Path) -> Result<Geo, String> {
    maxminddb::Reader::open_readfile(path)
        .map(Geo::Db)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn limiter_window() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut l = RateLimiter::new(ONLINE_PER_MINUTE, s(60));
        for i in 0..ONLINE_PER_MINUTE as u64 {
            assert!(l.try_acquire(t0 + s(i)), "request {i}");
        }
        // The 16th inside the minute waits until the first one ages out.
        assert!(!l.try_acquire(t0 + s(20)));
        assert!(!l.try_acquire(t0 + s(59)));
        assert!(l.try_acquire(t0 + s(60)));
        assert!(!l.try_acquire(t0 + s(60)));
        assert!(l.try_acquire(t0 + s(61)));
        // Never more than the limit in any 60 s.
        let mut l = RateLimiter::new(ONLINE_PER_MINUTE, s(60));
        let granted: Vec<u64> = (0..600).filter(|&i| l.try_acquire(t0 + s(i))).collect();
        for w in granted.windows(ONLINE_PER_MINUTE + 1) {
            assert!(w[ONLINE_PER_MINUTE] - w[0] >= 60, "{w:?}");
        }
        const { assert!(ONLINE_PER_MINUTE <= 45) };
    }

    #[test]
    fn limiter_pause() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut l = RateLimiter::new(15, s(60));
        assert!(l.try_acquire(t0));
        l.pause_until(t0 + s(30));
        l.pause_until(t0 + s(10)); // an earlier pause doesn't shorten it
        assert!(!l.try_acquire(t0 + s(29)));
        assert!(l.try_acquire(t0 + s(30)));
    }

    /// A fake ip-api.com: each connection gets the next of `replies`
    /// (`None`: closed unanswered; `Some("")`: held open, silent).
    fn fake_ip_api(
        replies: Vec<Option<&'static str>>,
    ) -> (Endpoint, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for (conn, reply) in l.incoming().zip(replies) {
                let mut c = conn.unwrap();
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                match reply {
                    Some("") => held.push(c),
                    Some(body) => {
                        let _ = write!(
                            c,
                            "HTTP/1.1 200 OK\r\nX-Rl: 14\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                    }
                    None => {}
                }
            }
        });
        (
            Endpoint::new(
                &format!("http://127.0.0.1:{port}/batch"),
                Duration::from_secs(5),
            )
            .unwrap(),
            hits,
        )
    }

    const BERLIN: &str =
        r#"[{"status":"success","lat":52.5,"lon":13.4,"city":"Berlin","countryCode":"DE"}]"#;

    fn settle(
        geo: &mut Geo,
        ips: &[IpAddr],
        cache: &mut HashMap<IpAddr, Option<Place>>,
        until: impl Fn(&HashMap<IpAddr, Option<Place>>) -> bool,
    ) {
        for _ in 0..100 {
            geo.resolve(ips, cache);
            if until(cache) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn online_never_waits_and_retries_after_failure() {
        let public = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        // A server that never answers: resolve still returns at once.
        let (ep, _) = fake_ip_api(vec![Some("")]);
        let mut geo = Geo::Online(Online::spawn(
            ep,
            RateLimiter::new(15, Duration::from_secs(60)),
            Duration::from_millis(50),
        ));
        let mut cache = HashMap::new();
        let t = Instant::now();
        for _ in 0..5 {
            geo.resolve(&[public], &mut cache);
        }
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        assert!(cache.is_empty());
        // Fails twice, then answers: the address is asked once and stays
        // queued until it is answered.
        let (ep, hits) = fake_ip_api(vec![None, None, Some(BERLIN)]);
        let mut geo = Geo::Online(Online::spawn(
            ep,
            RateLimiter::new(15, Duration::from_secs(60)),
            Duration::from_millis(50),
        ));
        let mut cache = HashMap::new();
        settle(&mut geo, &[public], &mut cache, |c| c.contains_key(&public));
        assert_eq!(
            cache.get(&public).cloned().flatten().map(|p| p.city),
            Some("Berlin".into())
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn known_and_private_addresses_are_not_sent() {
        let (ep, hits) = fake_ip_api(vec![Some(BERLIN)]);
        let mut geo = Geo::Online(Online::spawn(
            ep,
            RateLimiter::new(15, Duration::from_secs(60)),
            Duration::from_millis(50),
        ));
        let public = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7));
        let ula = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1));
        let mut cache = HashMap::new();
        cache.insert(public, None);
        geo.resolve(&[public, lan, ula], &mut cache);
        assert_eq!(cache.len(), 3);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(routable(public));
        assert!(!routable(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
    }

    #[test]
    fn ip_api_rows() {
        let ok = json!({"status": "success", "lat": 52.5, "lon": 13.4, "city": "Berlin", "countryCode": "DE"});
        let p = place_of(&ok).unwrap();
        assert_eq!((p.city.as_str(), p.country.as_str()), ("Berlin", "DE"));
        assert!(place_of(&json!({"status": "fail", "message": "private range"})).is_none());
    }

    #[test]
    fn database_search() {
        let dir = std::env::temp_dir().join(format!("quai-dash-geo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(find_db(std::slice::from_ref(&dir)), None);
        let other = dir.join("dbip-City-lite.mmdb");
        let _ = std::fs::write(&other, b"");
        assert_eq!(find_db(std::slice::from_ref(&dir)), Some(other));
        let lite = dir.join("GeoLite2-City.mmdb");
        let _ = std::fs::write(&lite, b"");
        assert_eq!(
            find_db(&[PathBuf::from("/nonexistent"), dir.clone()]),
            Some(lite)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
