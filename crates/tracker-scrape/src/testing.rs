//! In-process fake trackers for tests (feature `testing`).
//!
//! They behave like opentracker: zeros for unknown hashes, connection ids
//! checked on every UDP scrape. Knobs on [`FakeState`] inject the faults a
//! real tracker produces (lost packets, stray replies, refusals, expired
//! connection ids, truncated replies, HTTP errors and oversized bodies).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_bencode::Encoder;
use lodestar_metainfo::InfoHash;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;

use crate::Counts;

/// Counts for a hash the fake wasn't told about.
pub type Answer = fn(&InfoHash) -> Counts;

/// What a fake tracker knows and how it misbehaves. Shared with the test.
#[derive(Debug, Default)]
pub struct FakeState {
    pub swarms: Mutex<HashMap<InfoHash, Counts>>,
    /// Ignore this many incoming requests.
    pub drop_next: AtomicU32,
    /// Send a reply with a wrong transaction id before each real one.
    pub stray_replies: AtomicBool,
    /// Answer every scrape with this error.
    pub refuse: Mutex<Option<String>>,
    /// Forget every connection id issued so far (UDP), once.
    pub forget_connections: AtomicBool,
    /// Answer with at most this many entries.
    pub truncate_to: Mutex<Option<usize>>,
    /// HTTP status to answer with (0 means 200).
    pub status: AtomicU16,
    /// Pad HTTP replies past the body cap.
    pub oversize: AtomicBool,
    /// HTTP `flags.min_request_interval`.
    pub min_interval: Mutex<Option<u32>>,
    pub connects: AtomicU64,
    pub scrapes: AtomicU64,
    /// Hashes asked about, over all scrapes.
    pub hashes: AtomicU64,
    /// The last HTTP request path, without the query.
    pub last_path: Mutex<String>,
    /// Counts for hashes not [`FakeState::set`]; zeros without it.
    pub answer: Mutex<Option<Answer>>,
}

impl FakeState {
    pub fn set(&self, hash: InfoHash, counts: Counts) {
        self.swarms.lock().expect("fake lock").insert(hash, counts);
    }

    fn lookup(&self, hashes: &[InfoHash]) -> Vec<(InfoHash, Counts)> {
        self.scrapes.fetch_add(1, Ordering::Relaxed);
        self.hashes
            .fetch_add(hashes.len() as u64, Ordering::Relaxed);
        let swarms = self.swarms.lock().expect("fake lock");
        let answer = *self.answer.lock().expect("fake lock");
        let mut out: Vec<_> = hashes
            .iter()
            .map(|h| {
                let c = swarms.get(h).copied();
                (*h, c.or_else(|| answer.map(|f| f(h))).unwrap_or_default())
            })
            .collect();
        if let Some(n) = *self.truncate_to.lock().expect("fake lock") {
            out.truncate(n);
        }
        out
    }

    fn take_drop(&self) -> bool {
        let mut n = self.drop_next.load(Ordering::Relaxed);
        while n > 0 {
            match self.drop_next.compare_exchange_weak(
                n,
                n - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(now) => n = now,
            }
        }
        false
    }
}

/// A running fake tracker; stops when dropped.
#[derive(Debug)]
pub struct Fake {
    pub addr: SocketAddr,
    pub state: Arc<FakeState>,
    task: JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fake {
    /// `udp://…` or `http://…/announce`, ready for [`crate::TrackerUrl`].
    pub fn url(&self, udp: bool) -> String {
        if udp {
            format!("udp://{}", self.addr)
        } else {
            format!("http://{}/announce", self.addr)
        }
    }
}

/// A BEP 15 tracker on 127.0.0.1.
pub async fn udp_tracker() -> Fake {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind fake udp");
    let addr = socket.local_addr().expect("fake addr");
    let state = Arc::new(FakeState::default());
    let s = state.clone();
    let task = tokio::spawn(async move {
        let mut issued: Vec<u64> = Vec::new();
        let mut next_id = 0x1000u64;
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, from)) = socket.recv_from(&mut buf).await else {
                return;
            };
            let p = &buf[..n];
            if p.len() < 16 || s.take_drop() {
                continue;
            }
            if s.forget_connections.swap(false, Ordering::Relaxed) {
                issued.clear();
            }
            let action = u32::from_be_bytes(p[8..12].try_into().expect("4"));
            let tid = u32::from_be_bytes(p[12..16].try_into().expect("4"));
            let head = |action: u32, tid: u32| {
                let mut r = action.to_be_bytes().to_vec();
                r.extend_from_slice(&tid.to_be_bytes());
                r
            };
            let reply = match action {
                0 if p[..8] == 0x0417_2710_1980u64.to_be_bytes() => {
                    s.connects.fetch_add(1, Ordering::Relaxed);
                    next_id += 1;
                    issued.push(next_id);
                    let mut r = head(0, tid);
                    r.extend_from_slice(&next_id.to_be_bytes());
                    r
                }
                2 => {
                    let conn = u64::from_be_bytes(p[..8].try_into().expect("8"));
                    let refuse = s.refuse.lock().expect("fake lock").clone();
                    if !issued.contains(&conn) {
                        let mut r = head(3, tid);
                        r.extend_from_slice(b"connection id mismatch");
                        r
                    } else if let Some(msg) = refuse {
                        let mut r = head(3, tid);
                        r.extend_from_slice(msg.as_bytes());
                        r
                    } else {
                        let hashes: Vec<InfoHash> = p[16..]
                            .chunks_exact(20)
                            .map(|c| InfoHash(c.try_into().expect("20")))
                            .collect();
                        let mut r = head(2, tid);
                        for (_, c) in s.lookup(&hashes) {
                            for v in [c.seeders, c.completed, c.leechers] {
                                r.extend_from_slice(&v.to_be_bytes());
                            }
                        }
                        r
                    }
                }
                _ => continue,
            };
            if s.stray_replies.load(Ordering::Relaxed) {
                let mut stray = reply.clone();
                stray[4..8].copy_from_slice(&tid.wrapping_add(1).to_be_bytes());
                let _ = socket.send_to(&stray, from).await;
            }
            let _ = socket.send_to(&reply, from).await;
        }
    });
    Fake { addr, state, task }
}

/// A BEP 48 tracker on 127.0.0.1, one request per connection.
pub async fn http_tracker() -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake http");
    let addr = listener.local_addr().expect("fake addr");
    let state = Arc::new(FakeState::default());
    let s = state.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let s = s.clone();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") && req.len() < 64 << 10 {
                    match conn.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                if s.take_drop() {
                    return;
                }
                let line = String::from_utf8_lossy(&req);
                let target = line.split_whitespace().nth(1).unwrap_or("/");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                *s.last_path.lock().expect("fake lock") = path.to_owned();
                let hashes: Vec<InfoHash> = query
                    .split('&')
                    .filter_map(|kv| kv.strip_prefix("info_hash="))
                    .filter_map(|v| percent_decode(v)?.try_into().ok().map(InfoHash))
                    .collect();
                let status = match s.status.load(Ordering::Relaxed) {
                    0 => 200,
                    n => n,
                };
                let body = http_body(&s, &hashes);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: text/plain\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(head.as_bytes()).await;
                let _ = conn.write_all(&body).await;
                let _ = conn.shutdown().await;
            });
        }
    });
    Fake { addr, state, task }
}

fn http_body(s: &FakeState, hashes: &[InfoHash]) -> Vec<u8> {
    let mut out = Vec::new();
    let refuse = s.refuse.lock().expect("fake lock").clone();
    let min_interval = *s.min_interval.lock().expect("fake lock");
    let oversize = s.oversize.load(Ordering::Relaxed);
    let mut entries = s.lookup(hashes);
    entries.sort_by_key(|(h, _)| *h);
    entries.dedup_by_key(|(h, _)| *h);
    Encoder::new(&mut out).dict(|d| {
        if let Some(msg) = &refuse {
            d.entry(b"failure reason").str(msg);
            return;
        }
        d.entry(b"files").dict(|f| {
            for (h, c) in &entries {
                f.entry(&h.0).dict(|e| {
                    e.entry(b"complete").int(i64::from(c.seeders));
                    e.entry(b"downloaded").int(i64::from(c.completed));
                    e.entry(b"incomplete").int(i64::from(c.leechers));
                });
            }
        });
        if let Some(secs) = min_interval {
            d.entry(b"flags")
                .dict(|f| f.entry(b"min_request_interval").int(i64::from(secs)));
        }
        if oversize {
            d.entry(b"padding")
                .bytes(&vec![b'x'; crate::http::MAX_BODY]);
        }
    });
    out
}

fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}
