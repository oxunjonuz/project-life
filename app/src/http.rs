//! A very small HTTP/1.1 server: enough for the app's own window and nothing more.
//!
//! It binds to 127.0.0.1 on a port the OS chooses, and every request must carry the token that was
//! generated at launch. The UI is the only client; there is no other network code in this project
//! and the core itself never opens a connection.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn json_body(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
    pub fn param(&self, key: &str) -> Option<String> {
        self.query.get(key).cloned()
    }
    pub fn str_field(&self, key: &str) -> Option<String> {
        self.json_body().get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    }
    pub fn str_list(&self, key: &str) -> Vec<String> {
        self.json_body()
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    }
    pub fn u64_field(&self, key: &str) -> Option<u64> {
        self.json_body().get(key).and_then(|v| v.as_u64())
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(v: &serde_json::Value) -> Response {
        Response {
            status: 200,
            content_type: "application/json; charset=utf-8".into(),
            body: serde_json::to_vec_pretty(v).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }
    pub fn json_err(status: u16, msg: &str) -> Response {
        Response {
            status,
            content_type: "application/json; charset=utf-8".into(),
            body: serde_json::to_vec(&serde_json::json!({"error": msg})).unwrap_or_default(),
        }
    }
    pub fn asset(body: &'static str, content_type: &str) -> Response {
        Response { status: 200, content_type: content_type.into(), body: body.as_bytes().to_vec() }
    }
    /// A body that is not JSON, for the readers that are not web browsers.
    pub fn text(body: String, content_type: &str) -> Response {
        Response { status: 200, content_type: content_type.into(), body: body.into_bytes() }
    }
}

pub type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;

pub fn handle_conn(mut s: TcpStream, handler: &Handler) -> std::io::Result<()> {
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(20)));
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    let head_end;
    loop {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            head_end = pos + 4;
            break;
        }
        if buf.len() > 256 * 1024 {
            return Ok(());
        }
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut headers: HashMap<String, String> = HashMap::new();
    for l in lines {
        if l.is_empty() {
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_length {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target.clone(), HashMap::new()),
    };
    let req = Request { method, path, query, headers, body };
    let resp = handler(&req);
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        resp.status,
        status_text(resp.status),
        resp.content_type,
        resp.body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(&resp.body)?;
    let _ = s.flush();
    Ok(())
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_string_is_decoded() {
        let q = parse_query("at=2026-10-06T05%3A46%3A13.488Z&name=my+project&empty=");
        assert_eq!(q.get("at").unwrap(), "2026-10-06T05:46:13.488Z");
        assert_eq!(q.get("name").unwrap(), "my project");
        assert_eq!(q.get("empty").unwrap(), "");
    }

    #[test]
    fn percent_decoding_handles_the_forms_the_window_sends() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("%2Ftmp%2Fx"), "/tmp/x");
        assert_eq!(percent_decode("no-escapes_here"), "no-escapes_here");
        assert_eq!(percent_decode("%ZZ"), "%ZZ", "a broken escape is left alone, not dropped");
    }

    #[test]
    fn the_header_terminator_is_found() {
        assert_eq!(find_subslice(b"GET / HTTP/1.1\r\n\r\nbody", b"\r\n\r\n"), Some(14));
        assert_eq!(find_subslice(b"nothing", b"\r\n\r\n"), None);
    }
}
