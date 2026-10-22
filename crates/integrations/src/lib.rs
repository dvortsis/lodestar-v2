//! Integrations (§9, §13): download clients, notification channels, and the
//! two security pieces every integration shares.
//!
//! - [`clients`]: the [`DownloadClient`] trait and one adapter per client
//!   (`src/clients/<name>.rs`). qBittorrent is the reference adapter.
//! - [`notify`]: the [`Notifier`] trait, the shared retry loop and one
//!   sender per channel (`src/notify/<name>.rs`). The webhook is the
//!   reference sender.
//! - [`secret`]: [`Secret`] (never printed) and [`SecretStore`], which seals
//!   credentials at rest with a per-install key (§18.10).
//! - [`net`]: the SSRF guard. Every request to a user-configured URL goes
//!   through a [`GuardedClient`], which resolves DNS once, checks every
//!   address against a [`NetPolicy`], connects only to checked addresses and
//!   re-checks each redirect hop (§12).
//! - [`metrics`]: counters for the Serve stage of Insights (§10.9.5).
//!
//! `crates/integrations/README.md` describes the pattern a new adapter follows.

pub mod clients;
pub mod metrics;
pub mod net;
pub mod notify;
pub mod secret;

use std::future::Future;
use std::pin::Pin;

pub use clients::{
    AddOptions, AddOutcome, AuthFailure, Capabilities, Category, ClientConfig, ClientError,
    ClientInfo, ClientKind, DownloadClient, Feature, Magnet, MagnetError, Selection, TorrentFile,
};
pub use metrics::{IntegrationMetrics, MetricsSnapshot};
pub use net::{AddrClass, GuardedClient, NetError, NetPolicy, Resolve, SystemResolver};
pub use notify::{
    AttemptError, Channel, DeliveryError, DeliveryOutcome, Notification, NotificationItem,
    NotificationKind, Notifier, RetryPolicy, deliver,
};
pub use secret::{SealedSecret, Secret, SecretError, SecretStore};

/// A boxed, sendable future, so the traits stay object-safe
/// (`Box<dyn DownloadClient>`, `Box<dyn Notifier>`) without a macro crate.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The `User-Agent` every integration sends.
pub const USER_AGENT: &str = concat!("Lodestar/", env!("CARGO_PKG_VERSION"));

#[cfg(test)]
pub(crate) mod test_support {
    use std::io;
    use std::sync::{Arc, Mutex, OnceLock};

    /// Everything logged by any test in this binary.
    pub(crate) struct Logs(Arc<Mutex<Vec<u8>>>);

    impl Logs {
        pub(crate) fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    struct Writer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Writer {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Captures logs at every level, so a secret logged even at `trace`
    /// fails the test. The subscriber is global: a thread-local one loses
    /// events when other test threads cache a callsite as uninteresting.
    pub(crate) fn capture_logs() -> Logs {
        static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
        let buf = BUF.get_or_init(|| {
            let buf = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&buf);
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || Writer(Arc::clone(&sink)))
                .finish();
            tracing::subscriber::set_global_default(subscriber).unwrap();
            buf
        });
        Logs(Arc::clone(buf))
    }
}
