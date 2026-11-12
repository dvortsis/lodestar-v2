//! Condition waits with deadlines (standards §2): never a fixed sleep.

use std::time::Duration;

/// How often a condition is polled. This only paces the polling; the
/// deadline is what bounds the wait.
const POLL: Duration = Duration::from_millis(20);

/// Polls `check` until it returns `Ok`, and returns that. Panics at the
/// deadline with `what` and the last thing `check` reported.
pub async fn until<T>(
    what: &str,
    deadline: Duration,
    mut check: impl AsyncFnMut() -> Result<T, String>,
) -> T {
    assert!(deadline >= Duration::from_secs(10), "deadline under 10 s");
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let last = match check().await {
            Ok(v) => return v,
            Err(saw) => saw,
        };
        if tokio::time::Instant::now() >= end {
            panic!("timed out after {deadline:?} waiting for {what}: {last}");
        }
        tokio::time::sleep(POLL).await;
    }
}
