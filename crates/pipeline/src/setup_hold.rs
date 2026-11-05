//! The setup hold (LEGAL-2): on an instance nobody has set up yet,
//! nothing the crawler runs touches the network until setup completes,
//! so nobody's address joins the DHT before anyone has read the Legal
//! Notice.
//!
//! The binary hands [`crate::PipelineConfig::setup_done`] a receiver that
//! reads `true` once setup has completed. While it reads `false`:
//!
//! - **no source plugin starts:** the DHT doesn't bind its sockets or
//!   bootstrap, and no other source dials out (each one's task waits here,
//!   then runs as usual);
//! - **the scrape scheduler** asks no tracker and no DHT node;
//! - **the fetch pool starts paused**, so no metadata fetch or lookup runs.
//!
//! The plugins and the scheduler start on their own when it turns `true`.
//! The pool's pause is [`crate::Control`]'s, which lifts the hold and
//! then applies the user's own pause setting. A sender dropped while still
//! held keeps everything held (fail closed).

use tokio::sync::watch;

/// Whether `setup_done` holds the crawler now.
pub(crate) fn held(setup_done: Option<&watch::Receiver<bool>>) -> bool {
    setup_done.is_some_and(|rx| !*rx.borrow())
}

/// Returns once setup has completed. Never returns if the sender goes
/// away first: the hold stays on.
pub(crate) async fn released(mut setup_done: watch::Receiver<bool>) {
    if setup_done.wait_for(|done| *done).await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn released_waits_for_setup_and_a_dropped_sender_stays_held() {
        assert!(!held(None));
        let (tx, rx) = watch::channel(false);
        assert!(held(Some(&rx)));
        let waiter = tokio::spawn(released(rx.clone()));
        tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(10), waiter)
            .await
            .expect("released after setup")
            .unwrap();
        assert!(!held(Some(&rx)));

        // A closed channel answers at once, so one poll shows the outcome.
        let (tx, rx) = watch::channel(false);
        drop(tx);
        let mut wait = std::pin::pin!(released(rx));
        let polled =
            std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx))).await;
        assert!(polled.is_pending(), "a dropped sender keeps the hold");
    }
}
