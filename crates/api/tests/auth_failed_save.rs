//! A failed `auth.json` save changes nothing (§12, the M2 auth decisions):
//! every auth mutation that saves leaves the credentials, sessions, setup
//! state and proxy settings in effect exactly as they were, both on the
//! running instance and after a restart. A directory planted at
//! `auth.json.tmp` makes the save fail deterministically.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use lodestar_api::Auth;
use lodestar_api::auth::{AuthError, ProxyConfig, SetProxyError};

const OLD: &str = "correct horse battery";
const NEW: &str = "a different password";

/// A fresh address per password check, so the per-IP login limit never
/// decides an observation.
fn next_ip() -> IpAddr {
    static N: AtomicU32 = AtomicU32::new(1);
    IpAddr::V4(Ipv4Addr::from(
        0x0a00_0000 + N.fetch_add(1, Ordering::Relaxed),
    ))
}

fn cookie_header(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

fn tmp_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn accepts(auth: &Auth, password: &str) -> bool {
    auth.confirm(next_ip(), password).is_ok()
}

/// Everything a client or the owner can observe of the auth state.
#[derive(Debug, PartialEq)]
struct Seen {
    needs_setup: bool,
    token_pending: bool,
    session_valid: Option<bool>,
    old_accepted: bool,
    new_accepted: bool,
    proxy: ProxyConfig,
}

fn seen(auth: &Auth, session: Option<&str>) -> Seen {
    Seen {
        needs_setup: auth.needs_setup(),
        token_pending: auth.setup_token().is_some(),
        session_valid: session.map(|c| auth.verify(c)),
        old_accepted: accepts(auth, OLD),
        new_accepted: accepts(auth, NEW),
        proxy: auth.proxy(),
    }
}

fn proxy_on() -> ProxyConfig {
    ProxyConfig {
        enabled: true,
        trusted: vec!["10.0.0.0/8".into()],
        ..ProxyConfig::default()
    }
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Setup,
    ChangePassword,
    RotateSessions,
    SetProxy,
}

/// Every [`Auth`] method that saves `auth.json`.
const OPS: [Op; 4] = [
    Op::Setup,
    Op::ChangePassword,
    Op::RotateSessions,
    Op::SetProxy,
];

/// Runs `op`; on success returns the new session cookie it set, if any.
fn run(auth: &Auth, op: Op) -> Result<Option<String>, AuthError> {
    let ip = next_ip();
    match op {
        Op::Setup => {
            let token = auth.setup_token().expect("setup pending");
            auth.setup(ip, &token, OLD).map(Some)
        }
        Op::ChangePassword => auth.change_password(ip, OLD, NEW).map(Some),
        Op::RotateSessions => auth.rotate_sessions().map(Some),
        Op::SetProxy => match auth.set_proxy(proxy_on()) {
            Ok(_) => Ok(None),
            Err(SetProxyError::Storage(e)) => Err(e),
            Err(e) => panic!("{op:?}: unexpected {e:?}"),
        },
    }
}

/// A fixture `op` can run on: unclaimed for setup, claimed with [`OLD`]
/// (and its session cookie) for the rest.
fn fixture(op: Op) -> (tempfile::TempDir, PathBuf, Auth, Option<String>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let auth = Auth::load(&path, false).unwrap();
    let session = match op {
        Op::Setup => None,
        _ => {
            let token = auth.setup_token().unwrap();
            Some(cookie_header(&auth.setup(next_ip(), &token, OLD).unwrap()))
        }
    };
    (dir, path, auth, session)
}

/// For every mutation: error ⇒ observable state unchanged, before and
/// after a restart. Then, with the obstacle gone, the same call succeeds
/// and its result survives a restart.
#[test]
fn every_failed_save_leaves_the_state_unchanged() {
    for op in OPS {
        let (_dir, path, auth, session) = fixture(op);
        let file_before = std::fs::read(&path).unwrap();
        let before = seen(&auth, session.as_deref());

        std::fs::create_dir(tmp_path(&path)).unwrap();
        let err = run(&auth, op).expect_err("the save must fail");
        assert!(matches!(err, AuthError::Storage(_)), "{op:?}: {err:?}");

        assert_eq!(seen(&auth, session.as_deref()), before, "{op:?}: live");
        assert_eq!(std::fs::read(&path).unwrap(), file_before, "{op:?}: file");
        let restarted = Auth::load(&path, false).unwrap();
        assert_eq!(
            seen(&restarted, session.as_deref()),
            before,
            "{op:?}: after restart"
        );

        // The failed attempt didn't spend anything it needs: it works once
        // the save can.
        std::fs::remove_dir(tmp_path(&path)).unwrap();
        let new_session = run(&auth, op).unwrap_or_else(|e| panic!("{op:?}: {e:?}"));
        let after = seen(&auth, session.as_deref());
        assert_ne!(after, before, "{op:?}: success changes something");
        let restarted = Auth::load(&path, false).unwrap();
        assert_eq!(
            seen(&restarted, session.as_deref()),
            after,
            "{op:?}: success survives a restart"
        );
        if let Some(c) = new_session {
            let c = cookie_header(&c);
            assert!(
                auth.verify(&c) && restarted.verify(&c),
                "{op:?}: new cookie"
            );
        }
        assert!(!tmp_path(&path).exists(), "{op:?}: no temporary file left");
    }
}

/// The success paths, as before: what each mutation is meant to change.
#[test]
fn successful_saves_change_what_they_should() {
    for op in OPS {
        let (_dir, path, auth, session) = fixture(op);
        let before = seen(&auth, session.as_deref());
        run(&auth, op).unwrap();
        let want = match op {
            Op::Setup => Seen {
                needs_setup: false,
                token_pending: false,
                old_accepted: true,
                ..before
            },
            Op::ChangePassword => Seen {
                session_valid: Some(false),
                old_accepted: false,
                new_accepted: true,
                ..before
            },
            Op::RotateSessions => Seen {
                session_valid: Some(false),
                ..before
            },
            Op::SetProxy => Seen {
                proxy: proxy_on(),
                ..before
            },
        };
        assert_eq!(seen(&auth, session.as_deref()), want, "{op:?}: live");
        let restarted = Auth::load(&path, false).unwrap();
        assert_eq!(
            seen(&restarted, session.as_deref()),
            want,
            "{op:?}: after restart"
        );
    }
}

/// Area 2 review finding 1, first reproduction: a failed password change
/// used to end the caller's session and the old password, and accept the
/// new one until a restart.
#[test]
fn failed_password_save_preserves_active_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let auth = Auth::load(&path, false).unwrap();
    let ip = next_ip();
    let cookie = auth.setup(ip, &auth.setup_token().unwrap(), OLD).unwrap();
    let cookie = cookie_header(&cookie);

    std::fs::create_dir(tmp_path(&path)).unwrap();
    assert!(matches!(
        auth.change_password(ip, OLD, NEW),
        Err(AuthError::Storage(_))
    ));

    assert!(auth.verify(&cookie), "the session still works");
    assert!(accepts(&auth, OLD), "the old password still works");
    assert!(!accepts(&auth, NEW), "the new password doesn't");
    let restarted = Auth::load(&path, false).unwrap();
    assert!(restarted.verify(&cookie));
    assert!(accepts(&restarted, OLD) && !accepts(&restarted, NEW));
}

/// Area 2 review finding 1, second reproduction: a failed setup used to
/// leave the instance claimed with the token still pending, until a
/// restart returned it to setup.
#[test]
fn failed_setup_save_leaves_setup_pending() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let auth = Auth::load(&path, false).unwrap();
    let token = auth.setup_token().unwrap();

    std::fs::create_dir(tmp_path(&path)).unwrap();
    assert!(matches!(
        auth.setup(next_ip(), &token, OLD),
        Err(AuthError::Storage(_))
    ));

    assert!(auth.needs_setup());
    assert_eq!(auth.setup_token().as_deref(), Some(token.as_str()));
    assert_eq!(auth.login(next_ip(), OLD), Err(AuthError::NotSetUp));
    let restarted = Auth::load(&path, false).unwrap();
    assert!(restarted.needs_setup() && restarted.setup_token().is_some());

    // The same token claims it once the save can succeed.
    std::fs::remove_dir(tmp_path(&path)).unwrap();
    let cookie = cookie_header(&auth.setup(next_ip(), &token, OLD).unwrap());
    assert!(!auth.needs_setup() && auth.setup_token().is_none());
    assert!(auth.verify(&cookie));
}
