//! Peers from the operating system: the node process is the one listening
//! on the zone RPC port; its established TCP connections (other than RPC
//! clients) are its libp2p peers. Works the same for rs-quai and go-quai,
//! needs no node support, and sees TCP peers only (QUIC runs over one UDP
//! socket). Linux only; elsewhere the list stays empty with a note.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

use serde_json::{Value, json};

use crate::rpc::Endpoint;
use crate::state::Place;

/// One TCP connection of the node process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conn {
    /// Remote address.
    pub ip: IpAddr,
    /// Remote port.
    pub port: u16,
    /// Local port.
    pub local_port: u16,
    /// `in` or `out`.
    pub dir: &'static str,
}

struct Socket {
    local: (IpAddr, u16),
    remote: (IpAddr, u16),
    state: u8,
    inode: u64,
}

const ESTABLISHED: u8 = 0x01;
const LISTEN: u8 = 0x0a;

fn parse_addr(s: &str) -> Option<(IpAddr, u16)> {
    let (a, p) = s.split_once(':')?;
    let port = u16::from_str_radix(p, 16).ok()?;
    let ip = match a.len() {
        8 => IpAddr::V4(Ipv4Addr::from(u32::from_str_radix(a, 16).ok()?.swap_bytes())),
        32 => {
            // Four host-order u32 words.
            let mut b = [0u8; 16];
            for w in 0..4 {
                let v = u32::from_str_radix(&a[w * 8..w * 8 + 8], 16).ok()?;
                b[w * 4..w * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            let v6 = Ipv6Addr::from(b);
            match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(v6),
            }
        }
        _ => return None,
    };
    Some((ip, port))
}

fn sockets(path: &str) -> Vec<Socket> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Some(Socket {
                local: parse_addr(f.get(1)?)?,
                remote: parse_addr(f.get(2)?)?,
                state: u8::from_str_radix(f.get(3)?, 16).ok()?,
                inode: f.get(9)?.parse().ok()?,
            })
        })
        .collect()
}

fn socket_inodes(pid: u32) -> HashSet<u64> {
    let mut out = HashSet::new();
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return out };
    for e in dir.flatten() {
        if let Ok(target) = std::fs::read_link(e.path()) {
            let t = target.to_string_lossy();
            if let Some(n) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) {
                if let Ok(i) = n.parse() {
                    out.insert(i);
                }
            }
        }
    }
    out
}

/// The node's TCP peers, or why they can't be read.
pub fn connections(rpc_ports: &[u16]) -> Result<Vec<Conn>, String> {
    if !Path::new("/proc/net/tcp").exists() {
        return Err("peer map needs Linux /proc".into());
    }
    let mut all = sockets("/proc/net/tcp");
    all.extend(sockets("/proc/net/tcp6"));
    let Some(&zone_port) = rpc_ports.first() else { return Err("no RPC port".into()) };
    let listen_inode = all
        .iter()
        .find(|s| s.state == LISTEN && s.local.1 == zone_port)
        .map(|s| s.inode)
        .ok_or_else(|| format!("nothing listens on port {zone_port} here; run quai-dash on the node's host"))?;
    let mut pid = None;
    if let Ok(procs) = std::fs::read_dir("/proc") {
        for p in procs.flatten() {
            let Some(n) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if socket_inodes(n).contains(&listen_inode) {
                pid = Some(n);
                break;
            }
        }
    }
    let pid = pid.ok_or("the node process is not readable (run quai-dash as the node's user)")?;
    let mine = socket_inodes(pid);
    let listening: HashSet<u16> =
        all.iter().filter(|s| s.state == LISTEN && mine.contains(&s.inode)).map(|s| s.local.1).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in all.iter().filter(|s| s.state == ESTABLISHED && mine.contains(&s.inode)) {
        let ip = s.remote.0;
        if ip.is_loopback() || rpc_ports.contains(&s.local.1) || !seen.insert(ip) {
            continue;
        }
        // Accepted connections arrive on a listening port from an
        // ephemeral one; libp2p's port reuse makes outbound dials share the
        // listening port too, but then the remote port is its listener.
        let dir = if listening.contains(&s.local.1) && s.remote.1 >= 32768 { "in" } else { "out" };
        out.push(Conn { ip, port: s.remote.1, local_port: s.local.1, dir });
    }
    Ok(out)
}

/// IP geolocation: a local MaxMind database, or ip-api.com (opt-in).
pub enum Geo {
    /// Disabled.
    Off,
    /// GeoLite2/GeoIP2 City database.
    Db(maxminddb::Reader<Vec<u8>>),
    /// ip-api.com batch lookups (sends peer IPs to ip-api.com).
    Online(Endpoint),
}

impl Geo {
    /// `off`, `db` or `online`.
    pub fn kind(&self) -> &'static str {
        match self {
            Geo::Off => "off",
            Geo::Db(_) => "db",
            Geo::Online(_) => "online",
        }
    }

    /// Looks up `ips` not in `cache`, filling it (failures cache as `None`).
    pub fn resolve(&self, ips: &[IpAddr], cache: &mut HashMap<IpAddr, Option<Place>>) {
        let todo: Vec<IpAddr> = ips.iter().filter(|ip| !cache.contains_key(ip)).copied().collect();
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
            Geo::Online(ep) => {
                // ip-api allows 100 per batch and 15 batches a minute.
                for chunk in todo.chunks(100).take(2) {
                    let body = Value::Array(
                        chunk
                            .iter()
                            .map(|ip| json!({"query": ip.to_string(), "fields": "status,lat,lon,city,countryCode,query"}))
                            .collect(),
                    )
                    .to_string();
                    let Ok(resp) = ep.post(&body) else { return };
                    let Ok(Value::Array(rows)) = serde_json::from_slice::<Value>(&resp) else { return };
                    for (ip, row) in chunk.iter().zip(rows) {
                        let place = (row["status"] == "success").then(|| Place {
                            lat: row["lat"].as_f64().unwrap_or(0.0),
                            lon: row["lon"].as_f64().unwrap_or(0.0),
                            city: row["city"].as_str().unwrap_or("").to_string(),
                            country: row["countryCode"].as_str().unwrap_or("").to_string(),
                        });
                        cache.insert(*ip, place);
                    }
                }
            }
        }
    }
}

fn lookup_db(r: &maxminddb::Reader<Vec<u8>>, ip: IpAddr) -> Option<Place> {
    let res = r.lookup(ip).ok()?;
    let city: maxminddb::geoip2::City = res.decode().ok()??;
    let loc = city.location;
    Some(Place {
        lat: loc.latitude?,
        lon: loc.longitude?,
        city: city.city.names.english.map(str::to_string).unwrap_or_default(),
        country: city.country.iso_code.map(str::to_string).unwrap_or_default(),
    })
}

/// Opens a MaxMind database.
pub fn open_db(path: &Path) -> Result<Geo, String> {
    maxminddb::Reader::open_readfile(path).map(Geo::Db).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_addresses() {
        assert_eq!(parse_addr("0100007F:2328"), Some((IpAddr::V4(Ipv4Addr::LOCALHOST), 9000)));
        assert_eq!(
            parse_addr("0000000000000000FFFF00000100007F:0FA2"),
            Some((IpAddr::V4(Ipv4Addr::LOCALHOST), 4002))
        );
        assert_eq!(parse_addr("00000000000000000000000001000000:0050"), Some((IpAddr::V6(Ipv6Addr::LOCALHOST), 80)));
    }
}
