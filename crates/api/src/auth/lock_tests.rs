//! Changes hold the save lock across the disk; readers never wait on it
//! (F-auth-lock). The seams in [`super::hooks`] hold a save mid-way, so
//! each interleaving here is forced, not hoped for. Every wait has a
//! deadline and fails with what it saw.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::hooks::Point;
use super::*;

const OLD: &str = "correct horse battery";
/// Long enough for the slowest CI runner; only a failing test waits it out.
const DEADLINE: Duration = Duration::from_secs(20);

/// A fresh address per password check, so the login limit never decides
/// an outcome.
fn next_ip() -> IpAddr {
    static N: AtomicU32 = AtomicU32::new(1);
    IpAddr::V4(Ipv4Addr::from(
        0xc633_6400 + N.fetch_add(1, Ordering::Relaxed),
    ))
}

fn header(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

fn fresh() -> (tempfile::TempDir, Arc<Auth>) {
    let dir = tempfile::tempdir().unwrap();
    let auth = Auth::load(&dir.path().join("auth.json"), false).unwrap();
    (dir, Arc::new(auth))
}

/// Claimed with [`OLD`]; returns its session cookie too.
fn claimed() -> (tempfile::TempDir, Arc<Auth>, String) {
    let (dir, auth) = fresh();
    let token = auth.setup_token().unwrap();
    let cookie = header(&auth.setup(next_ip(), &token, OLD).unwrap());
    (dir, auth, cookie)
}

/// Reports every [`Point`] on the returned receiver, and holds the first
/// save that starts until the returned sender fires (or the deadline
/// passes, so a failing test can't hang).
fn hold_first_save(auth: &Auth) -> (Receiver<Point>, Sender<()>) {
    let (points, seen) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let held = AtomicBool::new(false);
    auth.disk.hooks.set(move |point| {
        let _ = points.send(point);
        if point == Point::Saving && !held.swap(true, Ordering::SeqCst) {
            let _ = released.lock().unwrap().recv_timeout(DEADLINE * 3);
        }
    });
    (seen, release)
}

/// Waits until `seen` has reported `locking` [`Point::Locking`]s and one
/// [`Point::Saving`]: the held save is in progress and the others have
/// passed their checks.
fn wait_for(seen: &Receiver<Point>, locking: usize) {
    let end = Instant::now() + DEADLINE;
    let mut got = Vec::new();
    while got.iter().filter(|p| **p == Point::Locking).count() < locking
        || !got.contains(&Point::Saving)
    {
        let left = end.saturating_duration_since(Instant::now());
        match seen.recv_timeout(left) {
            Ok(p) => got.push(p),
            Err(_) => panic!("wanted {locking} Locking and a Saving; saw {got:?}"),
        }
    }
}

/// Runs `f` on its own thread and waits for its answer. A reader stuck
/// behind the held save fails here instead of hanging.
fn within_deadline<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("{what} waited on a save in progress"))
}

/// The review's I1: while a change is stuck writing `auth.json` (an
/// fsync on a spun-down disk), every request's checks still answer, from
/// the state in effect before the change.
#[test]
fn readers_never_wait_on_a_save() {
    let (_dir, auth, old_cookie) = claimed();
    let (seen, release) = hold_first_save(&auth);
    let writer = {
        let auth = auth.clone();
        thread::spawn(move || auth.rotate_sessions())
    };
    wait_for(&seen, 1);

    let reader = auth.clone();
    let cookie = old_cookie.clone();
    let (valid, needs_setup, signs_in, ip, proxy) = within_deadline("a reader", move || {
        let peer = IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3));
        (
            reader.verify(&cookie),
            reader.needs_setup(),
            reader.proxy_signs_in(peer, &HeaderMap::new()),
            reader.client_ip(peer, &HeaderMap::new()),
            reader.proxy(),
        )
    });
    assert!(valid, "the old session is in effect until the swap");
    assert!(!needs_setup && !signs_in);
    assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)));
    assert_eq!(proxy, ProxyConfig::default());
    // Password checks don't wait either: Argon2 runs on a copy.
    let checker = auth.clone();
    let (login, confirm) = within_deadline("a sign-in", move || {
        (
            checker.login(next_ip(), OLD).is_ok(),
            checker.confirm(next_ip(), OLD).is_ok(),
        )
    });
    assert!(login && confirm);

    release.send(()).unwrap();
    let new_cookie = header(&writer.join().unwrap().unwrap());
    assert!(!auth.verify(&old_cookie) && auth.verify(&new_cookie));
}

/// Two password changes racing from the same old password: both pass
/// the check before either saves, and exactly one wins. The loser is
/// told its old password is wrong, which it now is.
#[test]
fn racing_password_changes_serialise() {
    let (dir, auth, _) = claimed();
    let (seen, release) = hold_first_save(&auth);
    let change = |new: &'static str| {
        let auth = auth.clone();
        thread::spawn(move || (new, auth.change_password(next_ip(), OLD, new)))
    };
    let a = change("first new password");
    let b = change("second new password");
    // One save is held; the other change has checked OLD and is queued
    // on the save lock.
    wait_for(&seen, 2);
    release.send(()).unwrap();

    let results = [a.join().unwrap(), b.join().unwrap()];
    let won: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).collect();
    assert_eq!(won.len(), 1, "exactly one change wins: {results:?}");
    let winner = won[0].0;
    let loser = results.iter().find(|(_, r)| r.is_err()).unwrap();
    assert_eq!(loser.1, Err(AuthError::BadPassword));

    let check = |auth: &Auth, password: &str| auth.confirm(next_ip(), password).is_ok();
    assert!(check(&auth, winner) && !check(&auth, loser.0) && !check(&auth, OLD));
    let restarted = Auth::load(&dir.path().join("auth.json"), false).unwrap();
    assert!(check(&restarted, winner) && !check(&restarted, loser.0));
}

/// Two setups racing with the right token: one claims the instance, the
/// other is told setup is done, and the winner's password is the one in
/// effect.
#[test]
fn racing_setups_serialise() {
    let (_dir, auth) = fresh();
    let token = auth.setup_token().unwrap();
    let (seen, release) = hold_first_save(&auth);
    let setup = |password: &'static str| {
        let (auth, token) = (auth.clone(), token.clone());
        thread::spawn(move || (password, auth.setup(next_ip(), &token, password)))
    };
    let a = setup("first owner password");
    let b = setup("second owner password");
    wait_for(&seen, 2);
    // Mid-save: still unclaimed, token still pending.
    assert!(auth.needs_setup() && auth.setup_token().is_some());
    release.send(()).unwrap();

    let results = [a.join().unwrap(), b.join().unwrap()];
    let won: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).collect();
    assert_eq!(won.len(), 1, "exactly one setup wins: {results:?}");
    let loser = results.iter().find(|(_, r)| r.is_err()).unwrap();
    assert_eq!(loser.1, Err(AuthError::SetupDone));
    assert!(!auth.needs_setup() && auth.setup_token().is_none());
    assert!(auth.confirm(next_ip(), won[0].0).is_ok());
    assert!(auth.confirm(next_ip(), loser.0).is_err());
    assert!(auth.verify(&header(won[0].1.as_ref().unwrap())));
}

/// The review's I2: a directory fsync that fails still saves, and says
/// so once, however many saves follow.
#[test]
fn a_failed_directory_fsync_warns_once() {
    let (dir, auth, _) = claimed();
    auth.disk.hooks.fail_dir_sync.store(true, Ordering::Relaxed);
    let event = &log_events::AUTH_DIR_SYNC_FAILED::EVENT;
    let before = event.count();
    let cookie = header(&auth.rotate_sessions().unwrap());
    auth.rotate_sessions().unwrap();
    auth.set_proxy(ProxyConfig::default()).unwrap();
    // This instance's own tally: the event's counter is shared by the
    // whole test binary, so it can only show the warning went out.
    let warnings = auth.disk.hooks.dir_sync_warnings.load(Ordering::Relaxed);
    assert_eq!(warnings, 1, "one warning for three saves");
    assert!(event.count() > before, "logged through the catalogue");
    assert!(!auth.verify(&cookie), "the saves applied");
    let restarted = Auth::load(&dir.path().join("auth.json"), false).unwrap();
    assert!(restarted.login(next_ip(), OLD).is_ok());
}

/// Setup's last check, that no password is in effect yet, is defensive:
/// a pending token implies none today. The [`Point::Locking`] hook stands
/// in for a future path that sets one while the token is still pending,
/// so the check is reached: setup is refused, nothing is saved, and the
/// password already in effect stays.
#[test]
fn setup_never_overwrites_a_password_already_in_effect() {
    let (dir, auth) = fresh();
    let token = auth.setup_token().unwrap();
    let planted = "password set some other way";
    let encoded = hash_password(planted);
    let weak = Arc::downgrade(&auth);
    let (points, seen) = mpsc::channel();
    auth.disk.hooks.set(move |point| {
        let _ = points.send(point);
        if point == Point::Locking
            && let Some(auth) = weak.upgrade()
        {
            auth.state.write().unwrap().password = Some(encoded.clone());
        }
    });

    let result = auth.setup(next_ip(), &token, "the setup's own password");
    assert_eq!(result, Err(AuthError::SetupDone));
    let points: Vec<_> = seen.try_iter().collect();
    assert_eq!(points, [Point::Locking], "refused before any save");
    assert!(auth.confirm(next_ip(), planted).is_ok());
    assert!(auth.confirm(next_ip(), "the setup's own password").is_err());
    assert_eq!(auth.setup_token(), Some(token), "the token isn't spent");
    let on_disk = Auth::load(&dir.path().join("auth.json"), false).unwrap();
    assert!(on_disk.needs_setup(), "nothing claimed on disk");
}
