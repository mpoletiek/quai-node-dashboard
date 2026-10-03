//! The web dashboard: one self-contained page plus `/api/state`.

use std::sync::{Arc, Mutex};

use tiny_http::{Header, Response, Server};

use crate::state::State;

/// The page (inline CSS/JS; Google Fonts with fallbacks). `web/index.html`
/// is a fragment (title, style, body content, script) so it can also be
/// published on its own, where it runs on demo data; here it gets a full
/// document around it.
pub const INDEX_HTML: &str = concat!(
    "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n",
    "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n",
    include_str!("../web/index.html"),
    "\n</html>\n"
);

fn header(k: &str, v: &str) -> Option<Header> {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).ok()
}

/// Serves until the process ends.
pub fn serve(listen: &str, state: Arc<Mutex<State>>) -> Result<(), String> {
    let server = Server::http(listen).map_err(|e| format!("listen {listen}: {e}"))?;
    for req in server.incoming_requests() {
        let url = req.url().to_string();
        let path = url.split('?').next().unwrap_or("/");
        let (body, ctype, status) = match path {
            "/" | "/index.html" => (INDEX_HTML.to_string(), "text/html; charset=utf-8", 200),
            "/api/state" => {
                let json = state
                    .lock()
                    .map(|s| serde_json::to_string(&*s).unwrap_or_default())
                    .unwrap_or_default();
                (json, "application/json", 200)
            }
            _ => ("not found".to_string(), "text/plain", 404),
        };
        let mut resp = Response::from_string(body).with_status_code(status);
        if let Some(h) = header("Content-Type", ctype) {
            resp.add_header(h);
        }
        if let Some(h) = header("Cache-Control", "no-store") {
            resp.add_header(h);
        }
        let _ = req.respond(resp);
    }
    Ok(())
}
