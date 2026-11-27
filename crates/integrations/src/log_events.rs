//! The integrations crate's log events (architecture §12.1). Client and
//! notifier URLs go through `lodestar_log::Url`, which removes their
//! credentials at every setting; nothing here ever carries a password, a
//! key, a cookie or a session id.

use crate::notify::{Channel, DeliveryError};

lodestar_log::catalogue! {
    "lodestar-integrations";
    /// One request to a download client, and its HTTP status.
    CLIENT_REQUEST {
        id: "integrations.client_request",
        component: Integrations,
        level: Debug,
        message: "A download client answered",
        fields: { client: Label, method: Label, status: Label },
    }
    /// The client didn't have the torrent's metadata in time, so it was
    /// left paused (or stopped) rather than started without it.
    CLIENT_LEFT_PAUSED {
        id: "integrations.client_left_paused",
        component: Integrations,
        level: Debug,
        message: "The download client didn't have the torrent's metadata in time; left it paused",
        fields: { client: Label },
    }
    /// Transmission older than labels: added without one.
    CLIENT_NO_LABELS {
        id: "integrations.client_no_labels",
        component: Integrations,
        level: Debug,
        message: "Transmission has no labels; added without one",
        fields: { rpc_version: Label },
    }
    /// The torrent was added but its label wasn't set.
    CLIENT_LABEL_FAILED {
        id: "integrations.client_label_failed",
        component: Integrations,
        level: Debug,
        message: "Transmission added the torrent but not its label",
        fields: { error: Error },
    }
    /// The loopback gate held back a request to a companion Lodestar runs
    /// because it isn't ready yet (X-cmp09-starting-status). Logged once
    /// per port per process.
    COMPANION_NOT_READY {
        id: "integrations.companion_not_ready",
        component: Integrations,
        level: Warn,
        message: "A request to a companion Lodestar runs was held back: it isn't ready yet",
        fields: { port: Label },
    }
    /// The OS store's roots, read once per process for routed TLS
    /// connections (`tls`): nothing failed and roots were kept. Counts
    /// and fixed words only, never a certificate, path or error text.
    TLS_ROOTS_LOADED {
        id: "integrations.tls_roots_loaded",
        component: Integrations,
        level: Info,
        message: "Loaded the certificate roots for routed connections",
        fields: { found: Size, load_errors: Size, unusable: Size, duplicates: Size, disallowed: Size, kept: Size, store: Label, disallowed_list: Label },
    }
    /// The same read, when a certificate or list couldn't be read or
    /// used, or no root was kept (routed connections then stay down, or
    /// fail with "the server's certificate isn't trusted"). The same
    /// fields as `integrations.tls_roots_loaded`.
    TLS_ROOTS_DEGRADED {
        id: "integrations.tls_roots_degraded",
        component: Integrations,
        level: Warn,
        message: "Some certificate roots for routed connections couldn't be read or used",
        fields: { found: Size, load_errors: Size, unusable: Size, duplicates: Size, disallowed: Size, kept: Size, store: Label, disallowed_list: Label },
    }
    /// One delivery attempt of a notification.
    NOTIFY_ATTEMPT {
        id: "integrations.notify_attempt",
        component: Integrations,
        level: Debug,
        message: "Notification attempt",
        fields: { channel: Label, status: Label, url: Url, port: Label, ok: Label },
    }
    /// An attempt failed and will be retried.
    NOTIFY_RETRY {
        id: "integrations.notify_retry",
        component: Integrations,
        level: Debug,
        message: "notification attempt failed; retrying",
        fields: { channel: Label, attempt: Label, error: Error, wait_ms: Size },
    }
    /// Every attempt failed.
    NOTIFY_FAILED {
        id: "integrations.notify_failed",
        component: Integrations,
        level: Info,
        message: "notification not delivered",
        fields: { channel: Label, attempts: Label, error: Error },
    }
    /// The secrets key file was readable by others and was restricted.
    SECRET_KEY_TIGHTENED {
        id: "integrations.secret_key_tightened",
        component: Integrations,
        level: Warn,
        message: "The secrets key was readable by other users; restricting it to 0600",
        fields: { path: Path, mode: Label },
    }
    /// The secrets key's restricted ACL couldn't be re-applied (Windows).
    SECRET_KEY_ACL_FAILED {
        id: "integrations.secret_key_acl_failed",
        component: Integrations,
        level: Warn,
        message: "Couldn't re-apply the secrets key's restricted ACL",
        fields: { path: Path, error: Error },
    }
}

/// A channel's name in a line.
pub fn channel(c: Channel) -> &'static str {
    match c {
        Channel::Webhook => "webhook",
        Channel::Ntfy => "ntfy",
        Channel::Discord => "discord",
        Channel::Email => "email",
        Channel::Rss => "rss",
    }
}

/// Every message is a fixed text or a number (an HTTP status, an SMTP
/// code, an address class): never an address, a URL or a secret.
impl lodestar_log::PlainError for DeliveryError {}
