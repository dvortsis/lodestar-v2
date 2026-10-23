//! Sign-in, keys and the trusted proxy's log events (architecture §12,
//! §12.1; B12's "Never logged or echoed").
//!
//! Client addresses are the `Client` kind, on in Private: the admin's own,
//! useful for security. Nothing a user typed is ever a field: not a
//! password, a key, its prefix or its hash, a feed token, a proxy user
//! name or a header. Why something failed is a fixed label.

use std::net::IpAddr;

use lodestar_log::{Client, Count, log_event};

use super::AuthError;

lodestar_log::catalogue! {
    "lodestar-api-auth";
    /// The admin password was set with the setup token, which is now
    /// spent.
    AUTH_SETUP_COMPLETE {
        id: "auth.setup_complete",
        component: Auth,
        level: Info,
        message: "auth: setup complete; the first-run token is spent",
        fields: {},
    }
    /// A sign-in with the password.
    AUTH_SIGN_IN {
        id: "auth.sign_in",
        component: Auth,
        level: Info,
        message: "Signed in",
        fields: { client: Client },
    }
    /// A password check failed: signing in, setting up, confirming a
    /// change or changing the password (`action`), and why (`reason`).
    AUTH_SIGN_IN_FAILED {
        id: "auth.sign_in_failed",
        component: Auth,
        level: Warn,
        message: "A password check failed",
        fields: { client: Client, action: Label, reason: Label },
    }
    /// An address used up its attempts (`limit`: `password`, `key_checks`
    /// or `key_requests`).
    AUTH_RATE_LIMITED {
        id: "auth.rate_limited",
        component: Auth,
        level: Warn,
        message: "Too many attempts from one address; it has to wait",
        fields: { client: Client, limit: Label },
    }
    /// The admin password changed; every other session ended.
    AUTH_PASSWORD_CHANGED {
        id: "auth.password_changed",
        component: Auth,
        level: Info,
        message: "The admin password was changed; other sessions ended",
        fields: { client: Client },
    }
    /// "Sign out other sessions".
    AUTH_SESSIONS_REVOKED {
        id: "auth.sessions_revoked",
        component: Auth,
        level: Info,
        message: "Every other session was signed out",
        fields: { client: Client },
    }
    /// Trusted-proxy sign-in was turned on or off, or its proxies
    /// changed: how many ranges are trusted, never who signs in.
    AUTH_PROXY_CHANGED {
        id: "auth.proxy_changed",
        component: Auth,
        level: Info,
        message: "Trusted-proxy sign-in settings changed",
        fields: { client: Client, enabled: Label, trusted: Label },
    }
    /// `LODESTAR_PROXY_AUTH=off` turned trusted-proxy sign-in off for this
    /// run.
    AUTH_PROXY_FORCED_OFF {
        id: "auth.proxy_forced_off",
        component: Auth,
        level: Warn,
        message: "LODESTAR_PROXY_AUTH is set: trusted-proxy sign-in is off for this run",
        fields: {},
    }
    /// `LODESTAR_PROXY_AUTH` is set to something other than `off`.
    AUTH_PROXY_OVERRIDE_IGNORED {
        id: "auth.proxy_override_ignored",
        component: Auth,
        level: Warn,
        message: "LODESTAR_PROXY_AUTH is set but not to `off`, so it changes nothing",
        fields: {},
    }
    /// `auth.json`'s trusted-proxy section doesn't check out: proxy
    /// sign-in is off. `field` names the setting, never its value.
    AUTH_PROXY_SETTINGS_INVALID {
        id: "auth.proxy_settings_invalid",
        component: Auth,
        level: Warn,
        message: "The saved trusted-proxy settings are invalid; proxy sign-in is off",
        fields: { field: Label },
    }
    /// Proxy sign-in trusts a range wider than /16 (IPv6 /48).
    AUTH_PROXY_WIDE_RANGE {
        id: "auth.proxy_wide_range",
        component: Auth,
        level: Warn,
        message: "Trusted-proxy sign-in trusts a range wider than /16 (IPv6 /48): every host in it can sign in",
        fields: {},
    }
    /// An API key or feed token was refused. `kind` is what it claimed to
    /// be (`api_key`, `feed_token` or `unknown`), `reason` why: never the
    /// secret, its prefix or its hash.
    AUTH_KEY_REFUSED {
        id: "auth.key_refused",
        component: Auth,
        level: Warn,
        message: "An API key or feed token was refused",
        fields: { client: Client, kind: Label, reason: Label },
    }
    /// A key's last use couldn't be recorded; the request went ahead.
    AUTH_KEY_USE_UNRECORDED {
        id: "auth.key_use_unrecorded",
        component: Auth,
        level: Warn,
        message: "An API key's last use couldn't be recorded",
        fields: {},
    }
}

/// What a password check was for.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Check {
    SignIn,
    Setup,
    Confirm,
    PasswordChange,
}

impl Check {
    fn label(self) -> &'static str {
        match self {
            Check::SignIn => "sign_in",
            Check::Setup => "setup",
            Check::Confirm => "confirm",
            Check::PasswordChange => "password_change",
        }
    }
}

/// Logs how a password check from `ip` ended.
pub(crate) fn checked<T>(ip: IpAddr, check: Check, result: &Result<T, AuthError>) {
    let client = Client(ip);
    match result {
        Ok(_) => match check {
            Check::SignIn => log_event!(AUTH_SIGN_IN, client = client),
            Check::Setup => log_event!(AUTH_SETUP_COMPLETE),
            Check::PasswordChange => log_event!(AUTH_PASSWORD_CHANGED, client = client),
            // Each confirmed change logs its own event.
            Check::Confirm => {}
        },
        Err(AuthError::RateLimited) => {
            log_event!(AUTH_RATE_LIMITED, client = client, limit = "password")
        }
        // A failed save is the server's, not the client's.
        Err(AuthError::Storage(_)) => {}
        Err(e) => log_event!(
            AUTH_SIGN_IN_FAILED,
            client = client,
            action = check.label(),
            reason = reason(e)
        ),
    }
}

fn reason(e: &AuthError) -> &'static str {
    match e {
        AuthError::BadPassword => "bad_password",
        AuthError::BadToken => "bad_token",
        AuthError::WeakPassword => "weak_password",
        AuthError::NotSetUp => "not_set_up",
        AuthError::SetupDone => "setup_done",
        AuthError::RateLimited => "rate_limited",
        AuthError::Storage(_) => "storage",
    }
}

/// A count as a label: how many ranges are trusted (`0` … `64`).
pub(crate) fn count(n: usize) -> Count {
    Count(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_a_fixed_reason() {
        for e in [
            AuthError::BadPassword,
            AuthError::BadToken,
            AuthError::WeakPassword,
            AuthError::NotSetUp,
            AuthError::SetupDone,
            AuthError::RateLimited,
        ] {
            assert!(!reason(&e).is_empty());
        }
        lodestar_log::check_ids(&[&CATALOGUE]).unwrap();
        // Nothing here carries a value a user typed: only clients and
        // labels.
        for e in CATALOGUE.events {
            for (_, k) in e.fields {
                assert!(
                    matches!(
                        k,
                        lodestar_log::FieldKind::Label
                            | lodestar_log::FieldKind::Data(lodestar_log::Kind::Client)
                    ),
                    "{}",
                    e.id
                );
            }
        }
    }
}
