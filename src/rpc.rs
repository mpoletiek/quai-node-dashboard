//! Minimal blocking HTTP/1.1 POST and JSON-RPC client (std only), so the
//! dashboard builds without the node's crates.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Most a reply's status line and headers may take.
const MAX_HEAD: u64 = 64 << 10;
/// Most headers a reply may have.
const MAX_HEADERS: usize = 100;
/// Most a reply's body may take. Node, stratum and geolocation replies are
/// kilobytes; a server that claims more is refused before anything is
/// allocated.
const MAX_BODY: usize = 16 << 20;

/// An HTTP reply.
#[derive(Clone, Debug, Default)]
pub struct Reply {
    /// Status code.
    pub code: u16,
    /// Header names (as the server spelled them) and values, in order.
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
}

impl Reply {
    /// The first header named `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// An `http://host:port/path` endpoint.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// The URL as given.
    pub url: String,
    host: String,
    port: u16,
    path: String,
    timeout: Duration,
}

impl Endpoint {
    /// Parses an `http://` URL.
    pub fn new(url: &str, timeout: Duration) -> Result<Endpoint, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("{url}: only http:// endpoints are supported"))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.trim_matches(|c| c == '[' || c == ']').to_string(),
                p.parse().map_err(|_| format!("{url}: bad port"))?,
            ),
            None => (authority.to_string(), 80),
        };
        Ok(Endpoint {
            url: url.to_string(),
            host,
            port,
            path,
            timeout,
        })
    }

    /// The port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// POSTs `body` and returns the response body of a 200.
    pub fn post(&self, body: &str) -> Result<Vec<u8>, String> {
        self.request("POST", &self.path, Some(body))
    }

    /// GETs `path` (with any query) on this host and returns the body of a 200.
    pub fn get(&self, path: &str) -> Result<Vec<u8>, String> {
        let base = self.path.trim_end_matches('/');
        self.request("GET", &format!("{base}{path}"), None)
    }

    /// GETs `path` and parses the JSON body.
    pub fn get_json(&self, path: &str) -> Result<Value, String> {
        serde_json::from_slice(&self.get(path)?).map_err(|e| e.to_string())
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<Vec<u8>, String> {
        let r = self.exchange(method, path, body)?;
        if r.code != 200 {
            return Err(format!("HTTP {}", r.code));
        }
        Ok(r.body)
    }

    /// POSTs `body` and returns the whole reply, whatever its status.
    pub fn post_reply(&self, body: &str) -> Result<Reply, String> {
        self.exchange("POST", &self.path, Some(body))
    }

    fn exchange(&self, method: &str, path: &str, body: Option<&str>) -> Result<Reply, String> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or_else(|| format!("{} does not resolve", self.host))?;
        let mut s = TcpStream::connect_timeout(&addr, self.timeout).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        let req = match body {
            Some(body) => format!(
                "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                self.host,
                self.port,
                body.len(),
            ),
            None => format!(
                "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
                self.host, self.port,
            ),
        };
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        // The read timeout bounds each read; the deadline bounds the whole
        // reply, so a server trickling a byte at a time can't hold the
        // caller.
        read_reply(
            Deadline {
                s,
                until: Instant::now() + self.timeout * 2,
            },
            MAX_BODY,
        )
    }

    /// Calls a JSON-RPC method and returns its `result`.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body =
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let v: Value = serde_json::from_slice(&self.post(&body)?).map_err(|e| e.to_string())?;
        if let Some(err) = v.get("error") {
            return Err(clip(
                err.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error"),
                200,
            ));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// A TCP stream whose reads fail once `until` has passed.
struct Deadline {
    s: TcpStream,
    until: Instant,
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "reply too slow"));
        }
        self.s.set_read_timeout(Some(left))?;
        self.s.read(buf)
    }
}

/// At most `n` characters of `s`, for text a server chose.
pub fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// One line of at most `max` bytes (with its line ending).
fn read_line_max(r: &mut impl BufRead, max: u64, what: &str) -> Result<String, String> {
    let mut buf = Vec::new();
    r.take(max)
        .read_until(b'\n', &mut buf)
        .map_err(|e| e.to_string())?;
    if buf.last() != Some(&b'\n') {
        return Err(if buf.len() as u64 >= max {
            format!("{what} too long")
        } else {
            format!("reply ended in the {what}")
        });
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Reads an HTTP/1.1 reply (status line, headers, body) of at most
/// `max_body` body bytes.
fn read_reply(r: impl Read, max_body: usize) -> Result<Reply, String> {
    let mut r = BufReader::new(r);
    let mut head = MAX_HEAD;
    let status = read_line_max(&mut r, head, "status line")?;
    head -= status.len() as u64;
    let code: u16 = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("bad HTTP status line: {}", clip(status.trim(), 80)))?;
    let mut len: Option<usize> = None;
    let mut chunked = false;
    let mut headers = Vec::new();
    loop {
        let raw = read_line_max(&mut r, head, "reply head")?;
        head -= raw.len() as u64;
        let line = raw.trim_end();
        if line.is_empty() {
            break;
        }
        if headers.len() == MAX_HEADERS {
            return Err("too many headers".into());
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
            let k = k.trim().to_ascii_lowercase();
            if k == "content-length" {
                len = Some(v.trim().parse().map_err(|_| "bad Content-Length")?);
            } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }
    }
    let too_big = || format!("reply larger than {} MiB", max_body >> 20);
    let mut data = Vec::new();
    if chunked {
        loop {
            let size = read_line_max(&mut r, 1024, "chunk size")?;
            let size = size.trim();
            let size = size.split(';').next().unwrap_or(size).trim();
            let n = usize::from_str_radix(size, 16).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if n > max_body - data.len() {
                return Err(too_big());
            }
            let got = (&mut r)
                .take(n as u64)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            let mut crlf = [0u8; 2];
            if got < n || r.read_exact(&mut crlf).is_err() {
                return Err("reply ended in a chunk".into());
            }
        }
    } else if let Some(n) = len {
        if n > max_body {
            return Err(too_big());
        }
        let got = (&mut r)
            .take(n as u64)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if got < n {
            return Err("reply shorter than its Content-Length".into());
        }
    } else {
        (&mut r)
            .take(max_body as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if data.len() > max_body {
            return Err(too_big());
        }
    }
    Ok(Reply {
        code,
        headers,
        body: data,
    })
}

/// `0x`-prefixed hex quantity → u64 (0 when absent or malformed).
pub fn hex_u64(v: &Value) -> u64 {
    v.as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

/// `0x`-prefixed hex quantity of any size → decimal string.
pub fn hex_dec(v: &Value) -> String {
    let Some(s) = v.as_str() else {
        return "0".into();
    };
    let digits = s.trim_start_matches("0x");
    // Quantities are at most 256 bits; anything longer is nonsense (and
    // the conversion below is quadratic in its length).
    if digits.len() > 64 {
        return "0".into();
    }
    // Base-10 conversion over u32 limbs (big-endian hex input).
    let mut limbs: Vec<u32> = vec![0];
    for c in digits.chars() {
        let Some(d) = c.to_digit(16) else {
            return "0".into();
        };
        let mut carry = u64::from(d);
        for l in limbs.iter_mut() {
            let x = u64::from(*l) * 16 + carry;
            *l = x as u32;
            carry = x >> 32;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
    }
    let mut out = Vec::new();
    while limbs.iter().any(|&l| l != 0) {
        let mut rem = 0u64;
        for l in limbs.iter_mut().rev() {
            let x = (rem << 32) | u64::from(*l);
            *l = (x / 10) as u32;
            rem = x % 10;
        }
        out.push(char::from(b'0' + rem as u8));
    }
    if out.is_empty() {
        return "0".into();
    }
    out.iter().rev().collect()
}

/// A wei amount (decimal string) as Quai with `decimals` fraction digits.
pub fn wei_to_quai(dec: &str, decimals: usize) -> String {
    let s = dec.trim_start_matches('0');
    let (int, frac) = if s.len() > 18 {
        (&s[..s.len() - 18], s[s.len() - 18..].to_string())
    } else {
        ("0", format!("{s:0>18}"))
    };
    let int = if int.is_empty() { "0" } else { int };
    if decimals == 0 {
        return int.to_string();
    }
    format!("{int}.{}", &frac[..decimals.min(18)])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn hex_decimal() {
        assert_eq!(hex_dec(&json!("0x0")), "0");
        assert_eq!(hex_dec(&json!("0xff")), "255");
        assert_eq!(
            hex_dec(&json!("0x3a25842919c3d855c49a31d")),
            "1124717808469535400940643101"
        );
        assert_eq!(hex_u64(&json!("0x9e8b2e")), 10_390_318);
        assert_eq!(wei_to_quai("1500000000000000000", 2), "1.50");
        assert_eq!(wei_to_quai("25", 3), "0.000");
        assert_eq!(hex_dec(&json!(format!("0x{}", "f".repeat(100_000)))), "0");
    }

    fn reply(raw: &[u8]) -> Result<Reply, String> {
        read_reply(raw, 1 << 20)
    }

    #[test]
    fn replies() {
        let r = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Rl: 3\r\n\r\nhello").unwrap();
        assert_eq!(
            (r.code, r.body.as_slice(), r.header("x-rl")),
            (200, &b"hello"[..], Some("3"))
        );
        let r = reply(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;x=y\r\nabc\r\n2\r\nde\r\n0\r\n\r\n")
            .unwrap();
        assert_eq!(r.body, b"abcde");
        let r = reply(b"HTTP/1.0 404 Not Found\r\n\r\nnope").unwrap();
        assert_eq!((r.code, r.body.as_slice()), (404, &b"nope"[..]));
    }

    #[test]
    fn hostile_replies_are_refused_without_allocating() {
        // A huge Content-Length is refused before reading.
        let e = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 17179869184\r\n\r\nx").unwrap_err();
        assert!(e.contains("larger"), "{e}");
        // A chunk size that would overflow, and one over the cap.
        for size in ["ffffffffffffffff", "fffffff"] {
            let raw = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{size}\r\nab");
            assert!(reply(raw.as_bytes()).unwrap_err().contains("larger"));
        }
        // A body with no length that runs past the cap.
        let mut raw = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        raw.extend(std::iter::repeat_n(b'x', (1 << 20) + 1));
        assert!(reply(&raw).unwrap_err().contains("larger"));
        // An endless header line, too many headers, a short body.
        let mut raw = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
        raw.extend(std::iter::repeat_n(b'a', 100_000));
        assert!(reply(&raw).unwrap_err().contains("too long"));
        let raw = format!("HTTP/1.1 200 OK\r\n{}\r\n", "A: b\r\n".repeat(101));
        assert!(reply(raw.as_bytes()).unwrap_err().contains("too many"));
        assert!(reply(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc").is_err());
        // A garbage status line is quoted, but clipped.
        let raw = format!("{}\r\n\r\n", "\u{e9}".repeat(500));
        assert!(reply(raw.as_bytes()).unwrap_err().chars().count() < 120);
    }

    #[test]
    fn trickling_server_hits_the_deadline() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n");
            for _ in 0..100 {
                if c.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let ep = Endpoint::new(
            &format!("http://127.0.0.1:{port}/"),
            Duration::from_millis(200),
        )
        .unwrap();
        let t = Instant::now();
        assert!(ep.post("{}").is_err());
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }
}
