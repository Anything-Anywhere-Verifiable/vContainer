//! A deliberately tiny HTTP/1.1 server for three endpoints. Every request is
//! answered and the connection closed; bodies are bounded.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use vcontainer_core::{unhex_array, CommitRequest, LaunchRequest, PORT};

use crate::app::{ApiError, App};

const MAX_HEAD: usize = 16 << 10;
const MAX_BODY: usize = 8 << 20;
const MAX_CONNS: usize = 32;

pub fn serve(app: Arc<App>) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", PORT)).context("bind API port")?;
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
            active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let (app, slot) = (app.clone(), Slot(active.clone()));
        std::thread::spawn(move || {
            let _slot = slot; // released even if the handler panics
            let _ = handle(stream, &app);
        });
    }
    Ok(())
}

struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn handle(mut s: TcpStream, app: &Arc<App>) -> Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(30)))?;
    let (method, path, body) = match read_request(&mut s) {
        Ok(r) => r,
        Err(e) => return respond(&mut s, 400, &error_json(&format!("{e:#}"))),
    };
    let result: Result<String, ApiError> = (|| {
        let (route, query) = path.split_once('?').unwrap_or((&path, ""));
        match (method.as_str(), route) {
            ("GET", "/v1/evidence") => {
                let nonce = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("nonce="))
                    .ok_or_else(|| ApiError(400, "missing nonce".into()))?;
                let ev = app.evidence(&unhex_array::<32>(nonce)?).map_err(|e| ApiError(500, format!("{e:#}")))?;
                Ok(serde_json::to_string(&ev)?)
            }
            ("POST", "/v1/launch") => {
                app.launch(serde_json::from_slice::<LaunchRequest>(&body)?)?;
                Ok("{}".into())
            }
            ("POST", "/v1/commit") => {
                app.commit(serde_json::from_slice::<CommitRequest>(&body)?)?;
                Ok("{}".into())
            }
            _ => Err(ApiError(404, "not found".into())),
        }
    })();
    match result {
        Ok(json) => respond(&mut s, 200, &json),
        Err(ApiError(code, msg)) => respond(&mut s, code, &error_json(&msg)),
    }
}

fn error_json(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

fn read_request(s: &mut TcpStream) -> Result<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_len = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            bail!("request head too large");
        }
        let n = s.read(&mut chunk)?;
        if n == 0 {
            bail!("connection closed");
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    req.parse(&buf[..head_len])?;
    let method = req.method.context("no method")?.to_string();
    let path = req.path.context("no path")?.to_string();
    let len: usize = req
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-length"))
        .map(|h| std::str::from_utf8(h.value).unwrap_or("0").trim().parse())
        .transpose()?
        .unwrap_or(0);
    if len > MAX_BODY {
        bail!("body too large");
    }
    let mut body = buf[head_len..].to_vec();
    body.truncate(len);
    while body.len() < len {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            bail!("truncated body");
        }
        body.extend_from_slice(&chunk[..n.min(len - body.len())]);
    }
    Ok((method, path, body))
}

fn respond(s: &mut TcpStream, code: u16, body: &str) -> Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    write!(
        s,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}
