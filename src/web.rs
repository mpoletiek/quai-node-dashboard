//! The web dashboard: one self-contained page plus `/api/state`, served by
//! a small HTTP/1.1 server (std only). It answers one request per
//! connection and bounds everything a client controls (connections,
//! request size, time), so a slow or hostile client can't tie up threads
//! or memory on the node's host. Requests must name this host (an IP
//! address, `localhost` or the machine's hostname), which keeps a web page
//! the operator visits from reading the dashboard through DNS rebinding.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::rpc::Deadline;
use crate::state::State;

/// The page (inline CSS/JS; fonts from `/fonts/`). `web/index.html`
/// is a fragment (title, style, body content, script) so it can also be
/// published on its own, where it runs on demo data; here it gets a full
/// document around it.
pub const INDEX_HTML: &str = concat!(
    "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n",
    "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n",
    include_str!("../web/index.html"),
    "\n</html>\n"
);

/// The page's fonts (`web/fonts`, SIL Open Font License), served from
/// `/fonts/<name>`: the dashboard loads nothing from other sites.
const FONTS: &[(&str, &[u8])] = &[
    (
        "barlow-condensed-500.woff2",
        include_bytes!("../web/fonts/barlow-condensed-500.woff2"),
    ),
    (
        "barlow-condensed-600.woff2",
        include_bytes!("../web/fonts/barlow-condensed-600.woff2"),
    ),
    (
        "barlow-condensed-700.woff2",
        include_bytes!("../web/fonts/barlow-condensed-700.woff2"),
    ),
    (
        "chakra-petch-400.woff2",
        include_bytes!("../web/fonts/chakra-petch-400.woff2"),
    ),
    (
        "chakra-petch-500.woff2",
        include_bytes!("../web/fonts/chakra-petch-500.woff2"),
    ),
    (
        "chakra-petch-600.woff2",
        include_bytes!("../web/fonts/chakra-petch-600.woff2"),
    ),
    (
        "share-tech-mono-400.woff2",
        include_bytes!("../web/fonts/share-tech-mono-400.woff2"),
    ),
    (
        "shippori-mincho-b1-800.woff2",
        include_bytes!("../web/fonts/shippori-mincho-b1-800.woff2"),
    ),
    (
        "shippori-mincho-b1-800-jp.woff2",
        include_bytes!("../web/fonts/shippori-mincho-b1-800-jp.woff2"),
    ),
    (
        "noto-sans-jp-jp.woff2",
        include_bytes!("../web/fonts/noto-sans-jp-jp.woff2"),
    ),
];

/// What the page may load: only this server (fonts, `/api/state`), and
/// its own inline script and style. Links out (the explorer) still open.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; font-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Connections handled at once; more are closed unanswered.
const MAX_CONNS: usize = 16;
/// Most a request's line and headers may take.
const MAX_HEAD: u64 = 8 << 10;
/// Most headers a request may have.
const MAX_HEADERS: usize = 64;
/// Time a client has to send its request.
const READ_TIME: Duration = Duration::from_secs(5);
/// Time each write of the reply may take.
const WRITE_TIME: Duration = Duration::from_secs(10);
/// How long a serialized state is reused: many tabs, or a client
/// hammering `/api/state`, cost at most four serializations a second.
const STATE_TTL: Duration = Duration::from_millis(250);

/// What the request handlers share.
struct Shared {
    state: Arc<Mutex<State>>,
    cache: Mutex<Option<(Instant, Arc<String>)>>,
    hostname: String,
}

impl Shared {
    /// The state as JSON, at most `STATE_TTL` old; `None` if the state's
    /// lock is poisoned.
    fn state_json(&self) -> Option<Arc<String>> {
        let mut cache = self.cache.lock().ok()?;
        if let Some((at, json)) = cache.as_ref()
            && at.elapsed() < STATE_TTL
        {
            return Some(json.clone());
        }
        let json = Arc::new(serde_json::to_string(&*self.state.lock().ok()?).ok()?);
        *cache = Some((Instant::now(), json.clone()));
        Some(json)
    }
}

/// Serves until the process ends.
pub fn serve(listen: &str, state: Arc<Mutex<State>>) -> Result<(), String> {
    let listener = TcpListener::bind(listen).map_err(|e| format!("listen {listen}: {e}"))?;
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_ascii_lowercase())
        .unwrap_or_default();
    serve_on(
        listener,
        Arc::new(Shared {
            state,
            cache: Mutex::new(None),
            hostname,
        }),
    );
    Ok(())
}

/// Counts a connection while it is open.
struct Open(Arc<AtomicUsize>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve_on(listener: TcpListener, shared: Arc<Shared>) {
    let open = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let Ok(conn) = conn else {
            // Out of file descriptors, say: don't spin.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
            open.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let guard = Open(open.clone());
        let shared = shared.clone();
        // A failed spawn drops the closure, the guard with it.
        let _ = std::thread::Builder::new()
            .name("web".into())
            .spawn(move || {
                let _guard = guard;
                let _ = handle(conn, &shared);
            });
    }
}

/// A request's method, path (without the query) and Host.
#[derive(Debug, PartialEq)]
struct Request {
    method: String,
    path: String,
    host: Option<String>,
}

/// Reads a request head of at most `MAX_HEAD` bytes; `Ok(None)` when it
/// is too large or malformed.
fn read_request(r: impl Read) -> io::Result<Option<Request>> {
    let mut r = BufReader::new(r).take(MAX_HEAD);
    let mut lines = Vec::new();
    loop {
        let mut buf = Vec::new();
        r.read_until(b'\n', &mut buf)?;
        if buf.last() != Some(&b'\n') {
            return Ok(None);
        }
        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
        if line.is_empty() {
            break;
        }
        if lines.len() > MAX_HEADERS {
            return Ok(None);
        }
        lines.push(line);
    }
    let mut first = lines
        .first()
        .map(|l| l.split_whitespace())
        .into_iter()
        .flatten();
    let (Some(method), Some(target)) = (first.next(), first.next()) else {
        return Ok(None);
    };
    let host = lines.iter().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("host")
            .then(|| v.trim().to_string())
    });
    Ok(Some(Request {
        method: method.to_string(),
        path: target.split('?').next().unwrap_or("/").to_string(),
        host,
    }))
}

/// Whether a Host header names this machine: an IP address, `localhost`,
/// or the hostname (bare or `.local`). A DNS-rebinding page can only send
/// a name its attacker controls, which is none of these.
fn host_allowed(host: &str, hostname: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or("", |(n, _)| n),
        None => host.split(':').next().unwrap_or(""),
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost"
        || name.parse::<std::net::IpAddr>().is_ok()
        || (!hostname.is_empty()
            && (name == hostname || name.strip_suffix(".local") == Some(hostname)))
}

/// A reply body: built in (the page, fonts), shared (the state's JSON),
/// or text built per request.
enum Body {
    Static(&'static [u8]),
    Shared(Arc<String>),
    Text(String),
}

impl Body {
    fn bytes(&self) -> &[u8] {
        match self {
            Body::Static(b) => b,
            Body::Shared(s) => s.as_bytes(),
            Body::Text(s) => s.as_bytes(),
        }
    }
}

fn route(req: Option<&Request>, shared: &Shared) -> (u16, &'static str, Body) {
    let text = |code, s: &str| (code, "text/plain; charset=utf-8", Body::Text(s.to_string()));
    let Some(req) = req else {
        return text(400, "bad request");
    };
    if req.method != "GET" && req.method != "HEAD" {
        return text(405, "method not allowed");
    }
    if !req
        .host
        .as_deref()
        .is_some_and(|h| host_allowed(h, &shared.hostname))
    {
        return text(
            403,
            "quai-dash answers only requests for this host: open it by IP address, localhost or the machine's hostname",
        );
    }
    match req.path.as_str() {
        "/" | "/index.html" => (
            200,
            "text/html; charset=utf-8",
            Body::Static(INDEX_HTML.as_bytes()),
        ),
        "/api/state" => match shared.state_json() {
            Some(json) => (200, "application/json", Body::Shared(json)),
            None => text(503, "state unavailable"),
        },
        path => match path
            .strip_prefix("/fonts/")
            .and_then(|name| FONTS.iter().find(|(n, _)| *n == name))
        {
            Some((_, bytes)) => (200, "font/woff2", Body::Static(bytes)),
            None => text(404, "not found"),
        },
    }
}

fn handle(conn: TcpStream, shared: &Shared) -> io::Result<()> {
    conn.set_write_timeout(Some(WRITE_TIME))?;
    let req = read_request(Deadline {
        s: conn.try_clone()?,
        until: Instant::now() + READ_TIME,
    })?;
    let (code, ctype, body) = route(req.as_ref(), shared);
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Service Unavailable",
    };
    let body = body.bytes();
    // Fonts never change within a build; everything else is live.
    let cache = if ctype == "font/woff2" {
        "public, max-age=604800"
    } else {
        "no-store"
    };
    let csp = if ctype.starts_with("text/html") {
        format!("Content-Security-Policy: {CSP}\r\n")
    } else {
        String::new()
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\n{csp}X-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut w = io::BufWriter::new(&conn);
    w.write_all(head.as_bytes())?;
    if req.is_some_and(|r| r.method != "HEAD") {
        w.write_all(body)?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn page_loads_only_bundled_fonts() {
        // Every font the page names is served, and nothing is fetched from
        // elsewhere.
        let urls: Vec<&str> = INDEX_HTML
            .split("url(\"fonts/")
            .skip(1)
            .filter_map(|r| r.split('"').next())
            .collect();
        assert_eq!(urls.len(), 10);
        for u in urls {
            assert!(
                FONTS.iter().any(|(n, b)| *n == u && b.starts_with(b"wOF2")),
                "{u}"
            );
        }
        assert!(!INDEX_HTML.contains("googleapis") && !INDEX_HTML.contains("gstatic"));
    }

    #[test]
    fn hosts() {
        for ok in [
            "127.0.0.1:8090",
            "localhost:8090",
            "LOCALHOST",
            "[::1]:8090",
            "10.0.0.13:8095",
            "node1",
            "node1.local:8090",
        ] {
            assert!(host_allowed(ok, "node1"), "{ok}");
        }
        for bad in [
            "evil.example:8090",
            "node1.evil.example",
            "127.0.0.1.nip.io",
            "",
            "[::1",
        ] {
            assert!(!host_allowed(bad, "node1"), "{bad}");
        }
        assert!(!host_allowed("anything", ""));
    }

    #[test]
    fn requests() {
        let r =
            read_request(&b"GET /api/state?since=3 HTTP/1.1\r\nHost: localhost:8090\r\n\r\n"[..])
                .unwrap();
        assert_eq!(
            r,
            Some(Request {
                method: "GET".into(),
                path: "/api/state".into(),
                host: Some("localhost:8090".into())
            })
        );
        let mut long = b"GET / HTTP/1.1\r\nX: ".to_vec();
        long.extend(std::iter::repeat_n(b'a', 100_000));
        assert_eq!(read_request(&long[..]).unwrap(), None);
        let many = format!("GET / HTTP/1.1\r\n{}\r\n", "A: b\r\n".repeat(65));
        assert_eq!(read_request(many.as_bytes()).unwrap(), None);
        assert_eq!(read_request(&b"\r\n"[..]).unwrap(), None);
    }

    fn get(port: u16, raw: &str) -> String {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(raw.as_bytes()).unwrap();
        let mut out = Vec::new();
        let _ = c.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn server() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let shared = Arc::new(Shared {
            state: Arc::new(Mutex::new(State::default())),
            cache: Mutex::new(None),
            hostname: "node1".into(),
        });
        std::thread::spawn(move || serve_on(l, shared));
        let ok = get(port, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(ok.starts_with("HTTP/1.1 200") && ok.contains("<!doctype html>"));
        assert!(ok.contains("X-Content-Type-Options: nosniff"));
        let st = get(port, "GET /api/state HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let body = st.split("\r\n\r\n").nth(1).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(body).is_ok());
        let head = get(port, "HEAD / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(head.starts_with("HTTP/1.1 200") && head.ends_with("\r\n\r\n"));
        assert!(
            get(port, "GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n").starts_with("HTTP/1.1 403")
        );
        assert!(get(port, "GET / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403"));
        assert!(
            get(port, "POST / HTTP/1.1\r\nHost: localhost\r\n\r\n").starts_with("HTTP/1.1 405")
        );
        assert!(
            get(port, "GET /etc/passwd HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .starts_with("HTTP/1.1 404")
        );
        assert!(
            get(
                port,
                "GET /fonts/../web.rs HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .starts_with("HTTP/1.1 404")
        );
        let font = get(
            port,
            "GET /fonts/chakra-petch-500.woff2 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(
            font.starts_with("HTTP/1.1 200")
                && font.contains("font/woff2")
                && font.contains("max-age")
        );
        assert!(ok.contains("Content-Security-Policy: default-src 'none'"));

        // Idle connections fill the slots; one more is closed unanswered,
        // and the server answers again once they time out.
        let idle: Vec<TcpStream> = (0..MAX_CONNS)
            .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
            .collect();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(get(port, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"), "");
        std::thread::sleep(READ_TIME + Duration::from_millis(500));
        assert!(get(port, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").starts_with("HTTP/1.1 200"));
        drop(idle);
    }
}
