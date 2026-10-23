//! `lodestar serve --fixture [DIR]` (build plan §1.3 seam 2): the real
//! server over a fixture data directory with known contents
//! (`lodestar_bench::fixture`), for screens, e2e tests, the v0.5 exit and
//! the user-doc screenshots.
//!
//! - **Where:** without `DIR`, a new temporary directory, removed on a
//!   clean exit. With `DIR`: built there if it's empty or absent, reused
//!   if it holds this build's `fixture.json`, and refused (exit 3, nothing
//!   written) if it holds anything else, so it can never touch a real
//!   install.
//! - **Offline:** the DHT is off (and locked off: a value saved in the
//!   app can't turn it on), as are blocklist files and subscriptions and
//!   their refresher. With no DHT there's no peer finder, so the fetch
//!   pool never fetches. Nothing else in `serve` reaches out today; a
//!   service that would must check [`OFFLINE`].
//! - **Local:** it binds `127.0.0.1:8090` unless `--config` or
//!   `LODESTAR_HTTP_BIND` names another address, and warns on one that
//!   isn't loopback.
//! - **Login:** the setup token is printed as usual. If
//!   `LODESTAR_FIXTURE_PASSWORD` is set, it claims the instance with that
//!   password first. There's no built-in password.
//!
//! Everything else (the UI, `/healthz`, `/api/v1/*`, the event stream) is
//! a normal `serve`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

#[cfg(feature = "fixture")]
pub use imp::serve;

/// Set while serving a fixture: services that would reach out stay off.
/// Every service that makes outbound requests must check it, as the
/// blocklist refresher does through [`unless_offline`]. TODO(#48): the
/// tracker scraper, once `services.rs` starts it; fixture swarm rows are
/// due for a scrape a few hours after the anchor.
static OFFLINE: AtomicBool = AtomicBool::new(false);

/// Whether this process serves a fixture (`/system`'s `fixture`).
pub fn active() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

/// The blocklist refresher's stop signal, already stopped while serving a
/// fixture (`services.rs`). Its first step reads the store, so a stopped
/// refresher returns before it could download anything.
pub fn unless_offline(stopped: watch::Receiver<bool>) -> watch::Receiver<bool> {
    gate(OFFLINE.load(Ordering::Relaxed), stopped)
}

fn gate(offline: bool, stopped: watch::Receiver<bool>) -> watch::Receiver<bool> {
    if offline {
        // A receiver whose sender is gone: `changed()` returns at once.
        watch::channel(true).1
    } else {
        stopped
    }
}

/// Built without the `fixture` feature: the flag is there, the data set
/// isn't.
#[cfg(not(feature = "fixture"))]
pub fn serve(_dir: Option<PathBuf>, _config_path: Option<&Path>) -> ExitCode {
    eprintln!("lodestar serve --fixture: this build has no fixture backend (feature `fixture`)");
    ExitCode::from(2)
}

#[cfg(feature = "fixture")]
mod imp {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use lodestar_bench::fixture as data;

    use super::*;
    use crate::config::Config;
    use crate::privdrop;

    /// The address it binds unless told otherwise.
    const BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8090);
    /// Claims the instance with this password, if set.
    const PASSWORD_VAR: &str = "LODESTAR_FIXTURE_PASSWORD";

    /// What `DIR` holds.
    #[derive(Debug, PartialEq, Eq)]
    enum Found {
        /// Absent or empty: build there.
        Empty,
        /// A fixture of this build, built at `anchor`: serve it as it is.
        Fixture { anchor: i64 },
        /// Anything else, with why.
        Foreign(String),
    }

    fn inspect(dir: &Path) -> Found {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Found::Empty,
            Err(e) => return Found::Foreign(format!("can't read it: {e}")),
        };
        if entries.count() == 0 {
            return Found::Empty;
        }
        let manifest = dir.join(data::MANIFEST);
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            return Found::Foreign(format!(
                "it isn't empty and has no {} (if it's a fixture build that was \
                 interrupted, delete the folder)",
                data::MANIFEST
            ));
        };
        let manifest = serde_json::from_str::<serde_json::Value>(&text).ok();
        let field = |k: &str| manifest.as_ref().and_then(|v| v.get(k)?.as_i64());
        match (field("format"), field("anchor")) {
            (Some(f), Some(anchor)) if f == i64::from(data::FORMAT) => {
                if marked(&dir.join(data::DB)) {
                    Found::Fixture { anchor }
                } else {
                    Found::Foreign(format!(
                        "it has a {} but its database isn't a fixture's",
                        data::MANIFEST
                    ))
                }
            }
            (Some(f), _) if f != i64::from(data::FORMAT) => Found::Foreign(format!(
                "its {} is format {f}; this build makes format {}: delete the folder to rebuild it",
                data::MANIFEST,
                data::FORMAT
            )),
            _ => Found::Foreign(format!("its {} isn't a fixture manifest", data::MANIFEST)),
        }
    }

    /// Whether `db` is a fixture's store of this format. Opened immutable:
    /// nothing is written beside it (no `-shm`, no lock), whatever it is.
    fn marked(db: &Path) -> bool {
        use lodestar_store::rusqlite::{Connection, OpenFlags, OptionalExtension};
        if !db.is_file() {
            return false;
        }
        let path: String = db
            .to_string_lossy()
            .chars()
            .map(|c| match c {
                '?' | '#' | '%' => format!("%{:02X}", c as u32),
                c => c.to_string(),
            })
            .collect();
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
        let Ok(conn) = Connection::open_with_flags(format!("file:{path}?immutable=1"), flags)
        else {
            return false;
        };
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [data::FORMAT_SETTING],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
            == Some(i64::from(data::FORMAT))
    }

    /// The config `serve --fixture` runs with: `--config` (if given) and the
    /// environment, over the fixture directory, with everything that reaches
    /// out turned off. `data/config.toml` and `$LODESTAR_DATA_DIR` are never
    /// read: they belong to a real install.
    fn config(
        dir: &Path,
        path: Option<&Path>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Config, String> {
        let shown = dir.to_string_lossy().into_owned();
        let mut c = Config::load(path, |k| {
            if k == "LODESTAR_DATA_DIR" {
                Some(shown.clone())
            } else {
                env(k)
            }
        })
        .map_err(|e| e.to_string())?;
        c.data_dir = dir.to_path_buf();
        c.dht.enabled = false;
        c.search.enabled = true;
        c.blocklist.files.clear();
        c.subscriptions.catalog.clear();
        c.subscriptions.lists.clear();
        if !c.layers.in_file("http.bind") && !c.layers.in_env("LODESTAR_HTTP_BIND") {
            c.http.bind = BIND;
        }
        // Read-only in the app, as an environment variable makes them: a
        // value saved there can't turn the DHT on or move the server.
        for var in ["LODESTAR_DHT_ENABLED", "LODESTAR_HTTP_BIND"] {
            if !c.layers.in_env(var) {
                c.layers.env.push(var);
            }
        }
        Ok(c)
    }

    /// A new, empty temporary directory.
    fn temp_dir() -> std::io::Result<PathBuf> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir =
            std::env::temp_dir().join(format!("lodestar-fixture-{}-{nanos}", std::process::id()));
        // Only its owner can read it: `auth.json` lives there.
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        Ok(dir)
    }

    /// Claims the instance with `password` unless it's claimed already.
    fn claim(config: &Config, password: &str) -> Result<bool, String> {
        let auth = lodestar_api::Auth::load(
            &config.data_dir.join("auth.json"),
            config.http.secure_cookies,
        )
        .map_err(|e| format!("loading auth: {e}"))?;
        let Some(token) = auth.setup_token() else {
            return Ok(false);
        };
        auth.setup(IpAddr::V4(Ipv4Addr::LOCALHOST), &token, password)
            .map_err(|e| format!("{PASSWORD_VAR}: {e}"))?;
        Ok(true)
    }

    /// Runs `serve --fixture`. Called before any thread starts: it drops root
    /// (PUID/PGID) itself, for the fixture's folder, once that folder passed
    /// the check.
    pub fn serve(dir: Option<PathBuf>, config_path: Option<&Path>) -> ExitCode {
        let (dir, temporary) = match dir {
            Some(d) => (d, false),
            None => match temp_dir() {
                Ok(d) => (d, true),
                Err(e) => {
                    eprintln!("lodestar serve --fixture: creating a temporary folder: {e}");
                    return ExitCode::FAILURE;
                }
            },
        };
        // Before anything is written or logged.
        let found = inspect(&dir);
        if let Found::Foreign(why) = &found {
            eprintln!(
                "lodestar serve --fixture: refusing {}: {why}. Name an empty or new folder.",
                dir.display()
            );
            return ExitCode::from(3);
        }
        let shown = dir.to_string_lossy().into_owned();
        let dropped = match privdrop::run(privdrop::Scope::DataDir, |k| {
            if k == "LODESTAR_DATA_DIR" {
                Some(shown.clone())
            } else {
                std::env::var(k).ok()
            }
        }) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("PUID/PGID: {e}");
                return ExitCode::FAILURE;
            }
        };
        let config = match config(&dir, config_path, |k| std::env::var(k).ok()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("config: {e}");
                return ExitCode::FAILURE;
            }
        };
        crate::init_logging(&config.log);
        dropped.log(&config.data_dir);
        if temporary {
            // Restarting in place would delete this folder and build a new
            // fixture, with a new key and setup token (B2c).
            crate::system::Restart::global().refuse(
                "This fixture is temporary and is deleted when Lodestar stops. Stop it and run \
                 `lodestar serve --fixture` again, or name a folder to keep one.",
            );
        }
        let outcome = run(&config, &found);
        if temporary {
            // The whole fixture lives here; nothing else was put in it.
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::warn!("removing {}: {e}", dir.display());
            }
        }
        match outcome {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("{e}");
                ExitCode::FAILURE
            }
        }
    }

    /// Built windows ("24 h", "7 d", cohort finality) hold this long.
    const STABLE_SECS: i64 = 6 * 3600;

    fn run(config: &Config, found: &Found) -> Result<(), String> {
        let dir = &config.data_dir;
        tracing::warn!(
            "fixture mode: test data in {}, not a real install",
            dir.display()
        );
        if let Found::Fixture { anchor } = *found {
            if crate::unix_now() > anchor + STABLE_SECS {
                tracing::warn!(
                    "this fixture was built more than six hours ago: its 24 h and 7 d numbers \
                     and cohort finality no longer match fixture.json; delete the folder to \
                     rebuild it"
                );
            }
        }
        if *found == Found::Empty {
            let anchor = data::anchor(crate::unix_now());
            tracing::info!(
                "building the fixture in {} (anchor {anchor})",
                dir.display()
            );
            let m = data::build(dir, anchor).map_err(|e| format!("building the fixture: {e}"))?;
            tracing::info!(
                "fixture built: {} torrents, {} shown by default; manifest {}",
                m.counts.total,
                m.counts.visible,
                dir.join(data::MANIFEST).display()
            );
        } else {
            tracing::info!("serving the fixture in {}", dir.display());
        }
        if let Some(password) = std::env::var(PASSWORD_VAR).ok().filter(|p| !p.is_empty()) {
            if claim(config, &password)? {
                tracing::info!("claimed with {PASSWORD_VAR}");
            } else {
                tracing::info!(
                    "{PASSWORD_VAR} ignored: this fixture is claimed already; sign in with its \
                     password"
                );
            }
        }
        let bind = config.http.bind;
        if !bind.ip().is_loopback() {
            tracing::warn!(
                "the fixture is served on {bind}, beyond this computer; it's test data, but anyone \
                 who can reach the address can try to sign in"
            );
        }
        OFFLINE.store(true, Ordering::Relaxed);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("starting the runtime: {e}"))?;
        runtime
            .block_on(crate::serve(config.clone()))
            .map_err(|e| e.to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
            move |k| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        }

        #[test]
        fn everything_that_reaches_out_is_off() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("c.toml");
            std::fs::write(
                &file,
                "data_dir = \"/srv/real\"\n[dht]\nenabled = true\n[blocklist]\nfiles = [\"x.txt\"]\n\
                 [subscriptions]\ncatalog = [\"x\"]\n[search]\nenabled = false\n",
            )
            .unwrap();
            let fx = dir.path().join("fx");
            let c = config(&fx, Some(&file), env(&[("LODESTAR_DATA_DIR", "/srv/real")])).unwrap();
            assert_eq!(c.data_dir, fx);
            assert!(!c.dht.enabled && c.search.enabled);
            assert!(c.blocklist.files.is_empty() && c.subscriptions.catalog.is_empty());
            assert_eq!(c.http.bind, BIND);
            assert!(
                c.layers.in_env("LODESTAR_DHT_ENABLED") && c.layers.in_env("LODESTAR_HTTP_BIND")
            );
            // An environment that turns the DHT on doesn't either.
            let c = config(&fx, None, env(&[("LODESTAR_DHT_ENABLED", "true")])).unwrap();
            assert!(!c.dht.enabled);
        }

        #[test]
        fn the_bind_follows_the_config_or_environment() {
            let dir = tempfile::tempdir().unwrap();
            let fx = dir.path().join("fx");
            let c = config(&fx, None, env(&[("LODESTAR_HTTP_BIND", "0.0.0.0:9000")])).unwrap();
            assert_eq!(c.http.bind, "0.0.0.0:9000".parse().unwrap());
            let file = dir.path().join("c.toml");
            std::fs::write(&file, "[http]\nbind = \"127.0.0.1:7000\"\n").unwrap();
            let c = config(&fx, Some(&file), env(&[])).unwrap();
            assert_eq!(c.http.bind, "127.0.0.1:7000".parse().unwrap());
        }

        #[test]
        fn a_real_data_dir_config_is_never_read() {
            // `$LODESTAR_DATA_DIR/config.toml` would be read without
            // `--config`; under --fixture it isn't.
            let real = tempfile::tempdir().unwrap();
            std::fs::write(
                real.path().join("config.toml"),
                "[http]\nbind = \"0.0.0.0:1\"\n",
            )
            .unwrap();
            let fx = tempfile::tempdir().unwrap();
            let shown = real.path().to_string_lossy().into_owned();
            let c = config(fx.path(), None, env(&[("LODESTAR_DATA_DIR", &shown)])).unwrap();
            assert_eq!(c.http.bind, BIND);
            assert_eq!(c.data_dir, fx.path());
        }

        #[test]
        fn only_empty_folders_and_fixtures_are_accepted() {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(inspect(&dir.path().join("new")), Found::Empty);
            assert_eq!(inspect(dir.path()), Found::Empty);
            std::fs::write(dir.path().join("lodestar.db"), "").unwrap();
            assert!(matches!(inspect(dir.path()), Found::Foreign(_)));
            let manifest = dir.path().join(data::MANIFEST);
            let good = format!("{{\"format\": {}, \"anchor\": 7200}}", data::FORMAT);
            std::fs::write(&manifest, &good).unwrap();
            // A stray manifest beside a store that isn't a fixture's.
            assert!(matches!(inspect(dir.path()), Found::Foreign(_)));
            std::fs::remove_file(dir.path().join("lodestar.db")).unwrap();
            let db = dir.path().join("lodestar.db");
            let mut store =
                lodestar_store::Store::open(&db, lodestar_store::StoreConfig::default()).unwrap();
            assert!(matches!(inspect(dir.path()), Found::Foreign(_)), "unmarked");
            store
                .set_setting_i64(data::FORMAT_SETTING, i64::from(data::FORMAT))
                .unwrap();
            drop(store);
            assert_eq!(inspect(dir.path()), Found::Fixture { anchor: 7200 });
            std::fs::write(&manifest, format!("{{\"format\": {}}}", data::FORMAT + 1)).unwrap();
            assert!(matches!(inspect(dir.path()), Found::Foreign(_)));
            std::fs::write(&manifest, "not json").unwrap();
            assert!(matches!(inspect(dir.path()), Found::Foreign(_)));
            // A manifest without its database isn't a fixture.
            std::fs::write(&manifest, &good).unwrap();
            std::fs::remove_file(dir.path().join("lodestar.db")).unwrap();
            assert!(matches!(inspect(dir.path()), Found::Foreign(_)));
        }

        #[test]
        fn the_refresher_is_stopped_only_while_offline() {
            let (_tx, rx) = watch::channel(false);
            assert!(!gate(false, rx).has_changed().unwrap(), "open while online");
            let (_tx, rx) = watch::channel(false);
            assert!(
                gate(true, rx).has_changed().is_err(),
                "closed while offline"
            );
        }
    }
}
