//! Waiting on a condition instead of a fixed sleep (CLAUDE.md
//! "Multi-session work": a slow runner gets the whole deadline, a fast one
//! goes on at once).

use std::time::Duration;

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
