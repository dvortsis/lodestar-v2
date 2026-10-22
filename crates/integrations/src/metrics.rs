//! Counters for the Serve stage of Insights (§10.9.5, `docs/metrics.md`
//! §6.7): "Sends to download clients" (successful sends) and notification
//! delivery outcomes. The pipeline reads a [`MetricsSnapshot`] into its
//! rollups; nothing here is persisted.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::clients::{AddOutcome, ClientError};
use crate::notify::DeliveryOutcome;

#[derive(Debug, Default)]
pub struct IntegrationMetrics {
    sends: AtomicU64,
    send_failures: AtomicU64,
    delivered: AtomicU64,
    delivery_failures: AtomicU64,
    delivery_retries: AtomicU64,
}

/// A point-in-time copy of the counters (monotonic since start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MetricsSnapshot {
    /// Torrents sent to download clients successfully: the `sends` field of
    /// the Serve rollup.
    pub sends: u64,
    /// Send requests that failed.
    pub send_failures: u64,
    pub notifications_delivered: u64,
    pub notification_failures: u64,
    /// Attempts beyond the first, delivered or not.
    pub notification_retries: u64,
}

impl IntegrationMetrics {
    /// Records one add request; a bulk send counts each torrent it carried.
    pub fn record_send(&self, result: &Result<AddOutcome, ClientError>) {
        match result {
            Ok(outcome) => self.sends.fetch_add(outcome.sent as u64, Ordering::Relaxed),
            Err(_) => self.send_failures.fetch_add(1, Ordering::Relaxed),
        };
    }

    pub fn record_delivery(&self, outcome: &DeliveryOutcome) {
        match outcome.result {
            Ok(()) => self.delivered.fetch_add(1, Ordering::Relaxed),
            Err(_) => self.delivery_failures.fetch_add(1, Ordering::Relaxed),
        };
        self.delivery_retries.fetch_add(
            u64::from(outcome.attempts.saturating_sub(1)),
            Ordering::Relaxed,
        );
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            sends: self.sends.load(Ordering::Relaxed),
            send_failures: self.send_failures.load(Ordering::Relaxed),
            notifications_delivered: self.delivered.load(Ordering::Relaxed),
            notification_failures: self.delivery_failures.load(Ordering::Relaxed),
            notification_retries: self.delivery_retries.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::Selection;
    use crate::notify::{Channel, DeliveryError};
    use std::time::Duration;

    #[test]
    fn counts_sends_and_deliveries() {
        let m = IntegrationMetrics::default();
        m.record_send(&Ok(AddOutcome {
            sent: 3,
            selection: Selection::None,
        }));
        m.record_send(&Err(ClientError::Timeout));
        m.record_delivery(&DeliveryOutcome {
            channel: Channel::Webhook,
            result: Ok(()),
            attempts: 2,
            elapsed: Duration::ZERO,
        });
        m.record_delivery(&DeliveryOutcome {
            channel: Channel::Webhook,
            result: Err(DeliveryError::Timeout),
            attempts: 4,
            elapsed: Duration::ZERO,
        });
        assert_eq!(
            m.snapshot(),
            MetricsSnapshot {
                sends: 3,
                send_failures: 1,
                notifications_delivered: 1,
                notification_failures: 1,
                notification_retries: 4,
            }
        );
    }
}
