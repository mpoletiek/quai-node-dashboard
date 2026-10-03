//! Minimal blocking HTTP/1.1 POST and JSON-RPC client (std only), so the
//! dashboard builds without the node's crates.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{Value, json};

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

    /// The same host on another port.
    pub fn with_port(&self, port: u16) -> Endpoint {
        Endpoint {
            url: format!("http://{}:{port}", self.host),
            port,
            path: "/".into(),
            ..self.clone()
        }
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
        let mut r = BufReader::new(s);
        let mut status = String::new();
        r.read_line(&mut status).map_err(|e| e.to_string())?;
        let code: u16 = status
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| format!("bad HTTP status line: {}", status.trim()))?;
        let mut len: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            r.read_line(&mut line).map_err(|e| e.to_string())?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().to_ascii_lowercase();
                if k == "content-length" {
                    len = v.trim().parse().ok();
                } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                    chunked = true;
                }
            }
        }
        let mut data = Vec::new();
        if chunked {
            loop {
                let mut size = String::new();
                r.read_line(&mut size).map_err(|e| e.to_string())?;
                let n = usize::from_str_radix(size.trim(), 16).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                let mut chunk = vec![0u8; n + 2];
                r.read_exact(&mut chunk).map_err(|e| e.to_string())?;
                data.extend_from_slice(&chunk[..n]);
            }
        } else if let Some(n) = len {
            data.resize(n, 0);
            r.read_exact(&mut data).map_err(|e| e.to_string())?;
        } else {
            r.read_to_end(&mut data).map_err(|e| e.to_string())?;
        }
        if code != 200 {
            return Err(format!("HTTP {code}"));
        }
        Ok(data)
    }

    /// Calls a JSON-RPC method and returns its `result`.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body =
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let v: Value = serde_json::from_slice(&self.post(&body)?).map_err(|e| e.to_string())?;
        if let Some(err) = v.get("error") {
            return Err(err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string());
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }
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
    }
}
