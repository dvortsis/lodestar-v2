//! `lodestar settings get|set|reset` on the real binary (§10.10.6): round
//! trips on a stopped store, and every refusal leaving the store as it was.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use lodestar_integrations::{Secret, SecretStore};
use lodestar_store::{Store, StoreConfig};

/// The secret the tests save, and look for in every output.
const SECRET: &str = "hunter2-do-not-print";

struct Data {
    dir: tempfile::TempDir,
}

impl Data {
    fn new() -> Data {
        let data = Data {
            dir: tempfile::tempdir().unwrap(),
        };
        drop(data.open());
        data
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn db(&self) -> PathBuf {
        self.path().join("lodestar.db")
    }

    fn open(&self) -> Store {
        Store::open(self.db(), StoreConfig::default()).unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lodestar"));
        c.arg("settings")
            .args(args)
            .env("LODESTAR_DATA_DIR", self.path())
            .env("LODESTAR_LOG", "warn")
            .env_remove("LODESTAR_HTTP_BIND")
            .env_remove("LODESTAR_SHOW_ADULT")
            .stdin(Stdio::null());
        c
    }

    /// Started as root (a CI container), it stays root here;
    /// `settings_run_as_puid` covers the drop.
    fn run(&self, args: &[&str]) -> Output {
        let out = self
            .command(args)
            .env("PUID", "0")
            .env("PGID", "0")
            .env("LODESTAR_ALLOW_ROOT", "1")
            .output()
            .unwrap();
        assert!(!text(&out).contains(SECRET), "{args:?}: {}", text(&out));
        out
    }

    /// Every row of the settings table.
    fn rows(&self) -> Vec<(String, String)> {
        let conn = lodestar_store::rusqlite::Connection::open(self.db()).unwrap();
        let mut stmt = conn
            .prepare("SELECT key, CAST(value AS TEXT) FROM settings ORDER BY key")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn saved(&self, key: &str) -> Option<String> {
        self.rows()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    fn put(&self, key: &str, value: &str) {
        self.open().set_settings_text(&[(key, value)]).unwrap();
    }

    /// A secret saved as the app saves it: the JSON string, sealed with
    /// the install's key for its field.
    fn save_secret(&self, section: &str, field: &str) {
        let store = SecretStore::open_or_create(self.path()).unwrap();
        let context = format!("settings.{section}.{field}");
        let json = serde_json::to_string(SECRET).unwrap();
        let sealed = store.seal(&context, &Secret::new(json)).unwrap();
        let hex: String = sealed
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.put(
            &context,
            &serde_json::to_string(&format!("sealed:{hex}")).unwrap(),
        );
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[track_caller]
fn status(o: &Output, code: i32) {
    assert_eq!(o.status.code(), Some(code), "{}", text(o));
}

#[track_caller]
fn says(o: &Output, what: &str) {
    assert!(text(o).contains(what), "wanted {what:?} in: {}", text(o));
}

#[test]
fn get_set_reset_round_trip() {
    let data = Data::new();
    let out = data.run(&["get"]);
    status(&out, 0);
    // Every registered section is listed.
    for s in [
        "general.data_dir",
        "network.http_bind",
        "crawler.paused",
        "content.show_adult",
        "integrations.torznab_api_key = (not set)",
        "notifications.email_password = (not set)",
        "preferences.theme",
        "logging.verbosity",
    ] {
        says(&out, s);
    }

    let out = data.run(&["set", "network.http_bind", "127.0.0.1:9321"]);
    status(&out, 0);
    says(&out, "Saved network.http_bind = \"127.0.0.1:9321\"");
    says(&out, "next start");
    assert_eq!(
        data.saved("settings.network.http_bind").as_deref(),
        Some("\"127.0.0.1:9321\"")
    );
    assert_eq!(
        data.saved("settings.network#revision").as_deref(),
        Some("1")
    );
    let out = data.run(&["get", "network.http_bind"]);
    status(&out, 0);
    says(&out, "network.http_bind = \"127.0.0.1:9321\"");
    says(&out, "source: saved");
    says(&out, "without the saved value:");

    // The same value again changes nothing, not even the revision.
    let out = data.run(&["set", "network.http_bind", "127.0.0.1:9321"]);
    status(&out, 0);
    says(&out, "nothing changed");
    assert_eq!(
        data.saved("settings.network#revision").as_deref(),
        Some("1")
    );

    // Typed values.
    status(&data.run(&["set", "crawler.paused", "true"]), 0);
    status(&data.run(&["set", "crawler.timeout_secs", "45"]), 0);
    status(&data.run(&["set", "content.show_adult", "false"]), 0);
    assert_eq!(
        data.saved("settings.crawler.paused").as_deref(),
        Some("true")
    );
    let out = data.run(&["get", "crawler"]);
    status(&out, 0);
    says(&out, "crawler.paused = true  [saved]");
    assert!(!text(&out).contains("network."), "{}", text(&out));

    let out = data.run(&["reset", "network.http_bind"]);
    status(&out, 0);
    says(&out, "Reset network.http_bind");
    assert_eq!(data.saved("settings.network.http_bind"), None);
    assert_eq!(
        data.saved("settings.network#revision").as_deref(),
        Some("2")
    );
    let out = data.run(&["reset", "network.http_bind"]);
    status(&out, 0);
    says(&out, "wasn't saved");

    // A saved value the environment hides, and a secret: --all clears
    // them too, and leaves everything outside `settings.` alone.
    data.put("settings.network.dht_port", "7000");
    data.save_secret("notifications", "email_password");
    data.put("marker", "kept");
    let out = data
        .command(&["get", "network.dht_port"])
        .env("PUID", "0")
        .env("PGID", "0")
        .env("LODESTAR_ALLOW_ROOT", "1")
        .env("LODESTAR_DHT_PORT", "7100")
        .output()
        .unwrap();
    status(&out, 0);
    says(&out, "environment, LODESTAR_DHT_PORT");
    let out = data.run(&["get", "notifications.email_password"]);
    status(&out, 0);
    says(&out, "notifications.email_password = (set)");
    let out = data
        .command(&["reset", "--all"])
        .env("PUID", "0")
        .env("PGID", "0")
        .env("LODESTAR_ALLOW_ROOT", "1")
        .env("LODESTAR_DHT_PORT", "7100")
        .output()
        .unwrap();
    status(&out, 0);
    says(&out, "Reset 5 saved values");
    let left: Vec<String> = data
        .rows()
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("settings.") && !k.contains('#'))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(data.saved("marker").as_deref(), Some("kept"));
    let out = data.run(&["reset", "--all"]);
    status(&out, 0);
    says(&out, "nothing changed");
}

/// Moving a secret's server resets the saved secret, as the API does,
/// and the command says so.
#[test]
fn a_secret_reset_by_its_server_is_named() {
    let data = Data::new();
    data.save_secret("integrations", "torznab_api_key");
    let out = data.run(&[
        "set",
        "integrations.torznab_url",
        "http://127.0.0.1:9117/api",
    ]);
    status(&out, 0);
    says(&out, "Also reset integrations.torznab_api_key");
    assert_eq!(data.saved("settings.integrations.torznab_api_key"), None);
    let out = data.run(&["get", "integrations.torznab_api_key"]);
    says(&out, "= (not set)");
}

/// Each refusal exits as `--help` says and changes nothing.
#[test]
fn refusals_change_nothing() {
    let data = Data::new();
    data.save_secret("integrations", "torznab_api_key");
    status(&data.run(&["set", "crawler.paused", "true"]), 0);
    let before = data.rows();
    let unchanged = |what: &str| assert_eq!(data.rows(), before, "{what}");

    // 3: the store is open (a running server).
    let held = data.open();
    let out = data.run(&["set", "crawler.paused", "false"]);
    status(&out, 3);
    says(&out, "open in another process");
    let out = data.run(&["get"]);
    status(&out, 3);
    drop(held);
    unchanged("store open");

    // 3: an update rollback is in progress.
    std::fs::create_dir_all(data.path().join("bin")).unwrap();
    std::fs::write(data.path().join("bin/rollback.json"), "{}").unwrap();
    for args in [
        &["get"][..],
        &["set", "crawler.paused", "false"],
        &["reset", "--all"],
    ] {
        let out = data.run(args);
        status(&out, 3);
        says(&out, "rollback");
    }
    std::fs::remove_file(data.path().join("bin/rollback.json")).unwrap();
    unchanged("rollback");

    // 3: a secret on the command line, never echoed (`run` checks).
    for field in [
        "integrations.torznab_api_key",
        "notifications.email_password",
        "notifications.webhook_url",
        "integrations.clients",
    ] {
        let out = data.run(&["set", field, SECRET]);
        status(&out, 3);
        says(&out, "holds a secret");
    }
    unchanged("secret");
    let out = data.run(&["get", "integrations.torznab_api_key"]);
    status(&out, 0);
    says(&out, "= (set)");

    // 3: privacy opt-ins and adult content need the app's confirmation.
    for (field, value) in [
        ("insights.query_logging", "true"),
        ("logging.data", "\"full\""),
        ("logging.console_detail", "false"),
    ] {
        let out = data.run(&["set", field, value]);
        status(&out, 3);
        says(&out, "confirm");
    }
    for field in [
        "content.show_adult",
        "content.torznab_adult",
        "content.stremio_adult",
        "content.export_adult",
    ] {
        let out = data.run(&["set", field, "true"]);
        status(&out, 3);
        says(&out, "18 or older");
    }
    unchanged("privacy and adult");

    // 2: names and values the API refuses, with its message.
    for (args, what) in [
        (
            &["set", "crawler.timeout_secs", "500"][..],
            "crawler.timeout_secs: Must be from 1 to 120.",
        ),
        (&["set", "network.dht_port", "70000"], "Not a valid value"),
        // A negative number is a value, not a flag: the API refuses it.
        (&["set", "logging.temporary_until", "-1"], "24 hours"),
        (
            &["set", "network.http_bind", "nowhere"],
            "network.http_bind:",
        ),
        (&["set", "crawler.paused", "\"yes\""], "Not a valid value"),
        (
            &["set", "network.secure_cookies", "true"],
            "can't be changed",
        ),
        (&["set", "general.data_dir", "\"/x\""], "can't be changed"),
        (&["set", "network.nope", "1"], "no setting \"nope\""),
        (&["set", "nope.x", "1"], "no settings section"),
        (&["set", "paused", "1"], "<section>.<field>"),
        (&["set", "crawler.paused", "null"], "reset"),
        (&["set", "content.confirm_adult", "true"], "no setting"),
        (&["reset", "general.data_dir"], "can't be changed"),
        (&["get", "nope"], "no settings section"),
    ] {
        let out = data.run(args);
        status(&out, 2);
        says(&out, what);
    }
    let out = data
        .command(&["set", "network.http_bind", "127.0.0.1:1"])
        .env("PUID", "0")
        .env("PGID", "0")
        .env("LODESTAR_ALLOW_ROOT", "1")
        .env("LODESTAR_HTTP_BIND", "127.0.0.1:2")
        .output()
        .unwrap();
    status(&out, 2);
    says(&out, "LODESTAR_HTTP_BIND");
    // clap's own: a missing value, `reset` with nothing to reset.
    status(&data.run(&["set", "crawler.paused"]), 2);
    status(&data.run(&["reset"]), 2);
    status(&data.run(&["reset", "crawler.paused", "--all"]), 2);
    unchanged("bad arguments");

    // No store: it fails, and creates none.
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .args(["settings", "get"])
        .env("LODESTAR_DATA_DIR", empty.path())
        .env("PUID", "0")
        .env("PGID", "0")
        .env("LODESTAR_ALLOW_ROOT", "1")
        .output()
        .unwrap();
    status(&out, 1);
    assert!(!empty.path().join("lodestar.db").exists());
}

/// A pending keep-or-revert record for the address is settled by `set`
/// and `reset` of it, and by `reset --all`; another field leaves it.
#[test]
fn the_address_timer_is_settled() {
    let data = Data::new();
    let key = "system.keep_or_revert.http_bind";
    let record = r#"{"kind":"http_bind","previous":"reset","new":"127.0.0.1:9401","state":"awaiting_confirmation","deadline":1}"#;
    for args in [
        &["set", "network.http_bind", "127.0.0.1:9402"][..],
        &["reset", "network.http_bind"],
        &["reset", "--all"],
    ] {
        data.put(key, record);
        let out = data.run(&["set", "crawler.paused", "true"]);
        status(&out, 0);
        assert!(data.saved(key).is_some(), "another field leaves it");
        let out = data.run(args);
        status(&out, 0);
        says(&out, "settled");
        assert_eq!(data.saved(key), None, "{args:?}");
    }
    // A refused change leaves it.
    data.put(key, record);
    status(&data.run(&["set", "network.http_bind", "nowhere"]), 2);
    assert!(data.saved(key).is_some());
}

/// Started as root, the command runs as PUID/PGID, so a file it leaves
/// in the data folder (the store's journal, `secrets.key`) is theirs.
#[cfg(unix)]
#[test]
fn settings_run_as_puid() {
    use std::os::unix::fs::MetadataExt;
    if !rustix::process::geteuid().is_root() {
        eprintln!("skipping: the PUID/PGID drop only happens when started as root");
        return;
    }
    let data = Data::new();
    data.save_secret("notifications", "email_password");
    let mut stack = vec![data.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        std::os::unix::fs::lchown(&dir, Some(4242), Some(4343)).unwrap();
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            std::os::unix::fs::lchown(e.path(), Some(4242), Some(4343)).unwrap();
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
        }
    }
    let as_puid = |args: &[&str]| {
        let out = data
            .command(args)
            .env("PUID", "4242")
            .env("PGID", "4343")
            .output()
            .unwrap();
        assert!(!text(&out).contains(SECRET), "{}", text(&out));
        out
    };
    status(&as_puid(&["get"]), 0);
    status(&as_puid(&["set", "crawler.paused", "true"]), 0);
    status(&as_puid(&["reset", "--all"]), 0);
    let mut stack = vec![data.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let m = std::fs::symlink_metadata(e.path()).unwrap();
            assert_eq!((m.uid(), m.gid()), (4242, 4343), "{}", e.path().display());
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
}
