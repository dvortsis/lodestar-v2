//! The real binary's maintenance commands: backup, rollback, vacuum,
//! reindex and stats, each with its refusals.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use lodestar_store::{Store, StoreConfig};

/// A data folder with a store, and the binary pointed at it.
struct Data {
    dir: tempfile::TempDir,
    port: u16,
}

impl Data {
    fn new() -> Data {
        let dir = tempfile::tempdir().unwrap();
        let data = Data {
            dir,
            // Nothing listens here, so "is a server up?" is always no.
            port: std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port(),
        };
        let store = data.open();
        set(&store, "marker", "first");
        drop(store);
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

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lodestar"))
            .args(args)
            .env("LODESTAR_DATA_DIR", self.path())
            .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{}", self.port))
            .env("LODESTAR_LOG", "warn")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn journal(&self) {
        std::fs::create_dir_all(self.path().join("bin")).unwrap();
        std::fs::write(self.path().join("bin/rollback.json"), "{}").unwrap();
    }

    fn marker(&self) -> String {
        get(&self.db(), "marker")
    }
}

fn set(store: &Store, key: &str, value: &str) {
    store
        .connection()
        .execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )
        .unwrap();
}

fn get(db: &Path, key: &str) -> String {
    let conn = lodestar_store::rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .unwrap()
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

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn backup_while_the_store_is_in_use() {
    let data = Data::new();
    std::fs::write(data.path().join("seal.key"), [7u8; 32]).unwrap();
    std::fs::write(data.path().join("config.toml"), "log = \"warn\"\n").unwrap();
    std::fs::create_dir_all(data.path().join("index/v1-t1")).unwrap();
    // The server's writer, mid-use: its latest commit is still in the WAL.
    let store = data.open();
    set(&store, "marker", "live");

    let out = data.run(&["backup"]);
    status(&out, 0);
    let says = text(&out);
    assert!(says.contains("search index isn't included"), "{says}");
    let backups = data.path().join("backups");
    let [folder] = entries(&backups).try_into().unwrap();
    assert!(folder.starts_with("lodestar-backup-"), "{folder}");
    let folder = backups.join(folder);
    assert_eq!(
        entries(&folder),
        ["README.txt", "config.toml", "lodestar.db", "seal.key"]
    );
    assert_eq!(get(&folder.join("lodestar.db"), "marker"), "live");
    assert_eq!(std::fs::read(folder.join("seal.key")).unwrap(), [7u8; 32]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&folder.join("seal.key")), 0o600);
        assert_eq!(mode(&folder.join("lodestar.db")), 0o600);
    }
    // The store itself is untouched and still usable by its writer.
    set(&store, "marker", "after");
    drop(store);
    assert_eq!(data.marker(), "after");

    // --to puts it elsewhere, and auth.json comes along when it exists.
    std::fs::write(data.path().join("auth.json"), "{}").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let to = elsewhere.path().join("nested");
    status(&data.run(&["backup", "--to", to.to_str().unwrap()]), 0);
    let [folder] = entries(&to).try_into().unwrap();
    assert!(entries(&to.join(folder)).contains(&"auth.json".to_string()));
}

#[test]
fn backup_refusals() {
    let data = Data::new();
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .args(["backup"])
        .env("LODESTAR_DATA_DIR", empty.path())
        .output()
        .unwrap();
    status(&out, 1);
    assert!(text(&out).contains("no store"), "{}", text(&out));
    assert!(entries(empty.path()).is_empty(), "nothing is created");

    data.journal();
    let out = data.run(&["backup"]);
    status(&out, 3);
    assert!(text(&out).contains("rollback.json"), "{}", text(&out));
    assert!(!data.path().join("backups").exists());
}

/// A snapshot as a migration takes it, then a change after it.
fn with_snapshot(data: &Data) -> PathBuf {
    let snapshot = data.path().join("lodestar.pre-v99.db");
    let store = data.open();
    store
        .connection()
        .execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
        .unwrap();
    set(&store, "marker", "second");
    drop(store);
    snapshot
}

#[test]
fn rollback_lists_and_restores_keeping_a_copy() {
    let data = Data::new();
    let out = data.run(&["rollback"]);
    status(&out, 0);
    assert!(
        text(&out).contains("No migration snapshots"),
        "{}",
        text(&out)
    );

    let snapshot = with_snapshot(&data);
    std::fs::create_dir_all(data.path().join("index/v1-t1")).unwrap();
    let out = data.run(&["rollback"]);
    status(&out, 0);
    assert!(text(&out).contains("lodestar.pre-v99.db"), "{}", text(&out));
    assert_eq!(data.marker(), "second", "listing changes nothing");

    let out = data.run(&["rollback", "v99", "--yes"]);
    status(&out, 0);
    assert_eq!(data.marker(), "first");
    assert!(snapshot.exists(), "the snapshot is kept");
    let names = entries(data.path());
    let kept = names
        .iter()
        .find(|n| n.starts_with("lodestar.before-rollback-"))
        .unwrap_or_else(|| panic!("{names:?}"));
    assert_eq!(get(&data.path().join(kept), "marker"), "second");
    assert!(!data.path().join("index").exists());
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("index.before-rollback-")),
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
    // The restored store opens and upgrades as usual.
    let store = data.open();
    set(&store, "marker", "third");
}

#[test]
fn rollback_refusals_change_nothing() {
    let data = Data::new();
    with_snapshot(&data);
    std::fs::write(data.path().join("lodestar.pre-v98.db"), b"not a database").unwrap();

    // No terminal and no --yes.
    let out = data.run(&["rollback", "v99"]);
    status(&out, 3);
    assert!(text(&out).contains("--yes"), "{}", text(&out));
    // The store is open (the server is running).
    let store = data.open();
    let out = data.run(&["rollback", "v99", "--yes"]);
    status(&out, 3);
    assert!(
        text(&out).contains("open in another process"),
        "{}",
        text(&out)
    );
    drop(store);
    // Not a snapshot of ours: names only, never paths.
    let outside = data.path().join("x").join("lodestar.pre-v99.db");
    status(
        &data.run(&["rollback", outside.to_str().unwrap(), "--yes"]),
        1,
    );
    status(&data.run(&["rollback", "v7", "--yes"]), 1);
    // A damaged snapshot.
    let out = data.run(&["rollback", "lodestar.pre-v98.db", "--yes"]);
    status(&out, 1);
    assert!(text(&out).contains("fails its check"), "{}", text(&out));
    // B-update's journal.
    data.journal();
    status(&data.run(&["rollback", "v99", "--yes"]), 3);
    status(&data.run(&["rollback"]), 3);

    assert_eq!(data.marker(), "second");
    assert!(
        !entries(data.path())
            .iter()
            .any(|n| n.contains("before-rollback") || n.ends_with(".tmp")),
        "{:?}",
        entries(data.path())
    );
}

#[test]
fn vacuum_compacts_offline_only() {
    let data = Data::new();
    let store = data.open();
    let blob = vec![0x5au8; 64 << 10];
    for i in 0..64 {
        store
            .connection()
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                (format!("junk{i}"), &blob),
            )
            .unwrap();
    }
    store
        .connection()
        .execute("DELETE FROM settings WHERE key LIKE 'junk%'", [])
        .unwrap();

    let out = data.run(&["vacuum"]);
    status(&out, 3);
    assert!(
        text(&out).contains("open in another process"),
        "{}",
        text(&out)
    );
    drop(store);

    let before = std::fs::metadata(data.db()).unwrap().len();
    let out = data.run(&["vacuum"]);
    status(&out, 0);
    assert!(text(&out).contains("freed"), "{}", text(&out));
    let after = std::fs::metadata(data.db()).unwrap().len();
    assert!(after < before / 2, "{before} -> {after}");
    assert_eq!(data.marker(), "first");
    assert!(!data.path().join("lodestar.db.vacuum.tmp").exists());
    // Settings that only a fresh file can change carry over.
    let store = data.open();
    let pragma = |p: &str| -> i64 {
        store
            .connection()
            .query_row(&format!("PRAGMA {p}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(pragma("page_size"), 16384);
    assert_eq!(pragma("auto_vacuum"), 2, "incremental");
    drop(store);

    data.journal();
    status(&data.run(&["vacuum"]), 3);
}

#[test]
fn reindex_marks_the_index_offline() {
    let data = Data::new();
    let out = data.run(&["stats", "--json"]);
    status(&out, 0);
    let stats: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let version = stats["index"]["version"].as_str().unwrap().to_string();
    let index = data.path().join("index");
    std::fs::create_dir_all(index.join(&version)).unwrap();
    std::fs::write(index.join(&version).join("seg"), b"x").unwrap();

    let store = data.open();
    let out = data.run(&["reindex"]);
    status(&out, 3);
    assert!(index.join(&version).exists());
    drop(store);

    let out = data.run(&["reindex"]);
    status(&out, 0);
    assert!(text(&out).contains("marked for rebuild"), "{}", text(&out));
    assert!(!index.join(&version).exists());
    assert!(index.join(format!("{version}.trash")).exists());
    let out = data.run(&["reindex"]);
    status(&out, 0);
    assert!(text(&out).contains("no search index yet"), "{}", text(&out));

    data.journal();
    status(&data.run(&["reindex"]), 3);
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .args(["reindex"])
        .env("LODESTAR_DATA_DIR", empty.path())
        .output()
        .unwrap();
    status(&out, 1);
}

#[test]
fn stats_read_while_running() {
    let data = Data::new();
    let store = data.open();
    let out = data.run(&["stats", "--json"]);
    status(&out, 0);
    let stats: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(stats["store"]["torrents"], 0);
    assert!(stats["store"]["schema"].as_i64().unwrap() > 0);
    assert_eq!(stats["index"]["state"], "missing");
    assert_eq!(stats["server_running"], false);
    assert!(stats["filesystem"]["kind"].is_string());
    let out = data.run(&["stats"]);
    status(&out, 0);
    assert!(text(&out).contains("Torrents    0"), "{}", text(&out));
    set(&store, "marker", "still writable");
    drop(store);

    data.journal();
    status(&data.run(&["stats"]), 3);
}

#[test]
fn bad_arguments_exit_2() {
    let data = Data::new();
    status(&data.run(&["frobnicate"]), 2);
    status(&data.run(&["vacuum", "--yes"]), 2);
    let out = data.run(&["--help"]);
    status(&out, 0);
    for command in ["serve", "backup", "rollback", "vacuum", "reindex", "stats"] {
        assert!(text(&out).contains(command), "{}", text(&out));
    }
}
