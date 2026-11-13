//! Waiting on a condition instead of a fixed sleep (CLAUDE.md
//! "Multi-session work": a slow runner gets the whole deadline, a fast one
//! goes on at once).

use std::time::Duration;

use lodestar_dht::NodeId;
use lodestar_dht::krpc::{self, Body, Message, Method, Query};
use lodestar_dht::node::Node;
use tokio::net::UdpSocket;

/// The shortest deadline a test waits with, so it passes on the slowest CI
/// runner (Windows).
pub const DEADLINE: Duration = Duration::from_secs(10);

/// Polls `cond` every 20 ms until it holds (`true`) or `timeout` passes
/// (`false`). The caller asserts on the result, naming what it waited for.
pub async fn wait_for(mut cond: impl AsyncFnMut() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until `node` has run a maintenance tick that started after this
/// call, then until that tick has finished. A positive event stands in
/// for a fixed wait before a check that something did *not* happen:
/// sampling and held lookups start only in a tick, so a tick that ran
/// while paused, and did nothing, is what the check needs.
///
/// The signal: a ping from a stranger that never answers makes the node
/// ping back to verify it; the node expires that ping a query timeout
/// later, early in a tick and before sampling or lookups start. The snapshot then
/// waits for the rest of the tick, because the node's actor is one task
/// that serves a command only between ticks and datagrams.
#[allow(dead_code)] // Not every test binary that includes this module uses it.
pub async fn a_tick_ran(node: &Node) {
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let to = node.local_addrs()[0];
    let before = node.stats().timeouts;
    let ping = Message {
        transaction_id: &[0, 9],
        version: None,
        ip: None,
        // Not read-only, so the node verifies us.
        read_only: false,
        body: Body::Query(Query {
            id: NodeId([0x3c; 20]),
            method: Method::Ping,
        }),
    }
    .encode();
    stranger.send_to(&ping, to).await.unwrap();
    let mut buf = vec![0u8; 2048];
    let pinged_back = tokio::time::timeout(DEADLINE, async {
        loop {
            let (n, from) = stranger.recv_from(&mut buf).await.unwrap();
            if from == to
                && let Ok(m) = krpc::parse(&buf[..n])
                && matches!(m.body, Body::Query(_))
            {
                return;
            }
        }
    })
    .await;
    assert!(
        pinged_back.is_ok(),
        "the node didn't ping the stranger back within 10 s"
    );
    assert!(
        wait_for(async || node.stats().timeouts > before, DEADLINE).await,
        "the node expired no query within 10 s: timeouts still {} (from {before})",
        node.stats().timeouts
    );
    node.snapshot().await.expect("running");
}
