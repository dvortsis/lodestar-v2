//! Loopback throughput and memory per connection.
//!
//! ```sh
//! cargo run --release -p lodestar-utp --example bench [megabytes] [connections]
//! ```
//!
//! Memory is the change in resident set size (Linux only) while holding
//! idle connections open, divided by the endpoints (both ends live in this
//! process). Configured buffers are allocated as data arrives, so this is
//! the idle cost; the bound under load is in the output too.

use std::time::Instant;

use lodestar_utp::{Config, UtpSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let megabytes: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(256);
    let connections: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1000);
    let any = "127.0.0.1:0".parse().unwrap();

    // Throughput: one connection, one direction.
    let server = UtpSocket::bind(any).await.unwrap();
    let mut listener = server.listen(4);
    let addr = server.local_addr().unwrap();
    let sink = tokio::spawn(async move {
        let mut s = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1 << 16];
        let mut total = 0usize;
        loop {
            let n = s.read(&mut buf).await.unwrap();
            if n == 0 {
                break total;
            }
            total += n;
        }
    });
    let client = UtpSocket::bind(any).await.unwrap();
    let mut c = client.connect(addr).await.unwrap();
    let chunk = vec![0x5au8; 1 << 16];
    let started = Instant::now();
    for _ in 0..megabytes * 16 {
        c.write_all(&chunk).await.unwrap();
    }
    c.shutdown().await.unwrap();
    let total = sink.await.unwrap();
    let secs = started.elapsed().as_secs_f64();
    println!(
        "throughput: {} MiB in {:.2} s = {:.1} MiB/s ({} retransmits)",
        total >> 20,
        secs,
        total as f64 / secs / f64::from(1 << 20),
        client.stats().retransmits,
    );
    drop(c);

    // Memory: idle connections.
    let config = Config {
        max_connections: connections + 16,
        ..Config::default()
    };
    let server = UtpSocket::bind_with(any, config.clone()).await.unwrap();
    let mut listener = server.listen(connections);
    let addr = server.local_addr().unwrap();
    let client = UtpSocket::bind_with(any, config.clone()).await.unwrap();
    let before = rss_bytes();
    let mut held = Vec::with_capacity(connections * 2);
    for _ in 0..connections {
        let mut c = client.connect(addr).await.unwrap();
        c.write_all(b"x").await.unwrap();
        let mut s = listener.accept().await.unwrap();
        let mut one = [0u8; 1];
        s.read_exact(&mut one).await.unwrap();
        held.push(c);
        held.push(s);
    }
    let after = rss_bytes();
    match (before, after) {
        (Some(b), Some(a)) => println!(
            "memory: {} idle connections (both ends) add {} KiB RSS = {} bytes per endpoint",
            connections,
            a.saturating_sub(b) >> 10,
            a.saturating_sub(b) / held.len() as u64,
        ),
        _ => println!("memory: RSS not available on this platform"),
    }
    println!(
        "bound under load: {} KiB per connection (2 × {} KiB receive window + {} KiB send buffer)",
        (2 * config.recv_window as usize + config.send_buffer) >> 10,
        config.recv_window >> 10,
        config.send_buffer >> 10,
    );
}
