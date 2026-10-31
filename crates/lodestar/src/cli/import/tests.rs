use std::path::Path;

use clap::Parser;
use lodestar_store::StoreConfig;
use lodestar_store::rusqlite::{Connection, params};

use super::*;
use crate::cli::{Cli, Command};

fn parse(s: &str) -> Action {
    let cli = Cli::try_parse_from(std::iter::once("lodestar").chain(s.split_whitespace()))
        .unwrap_or_else(|e| panic!("{s}: {e}"));
    match cli.command {
        Some(Command::Import(a)) => a.action,
        c => panic!("{c:?}"),
    }
}

fn source(kind: SourceKind) -> SourceArgs {
    SourceArgs {
        source: kind,
        path: None,
        allow_shares: Vec::new(),
        host: None,
        port: None,
        dbname: None,
        user: None,
        password_file: None,
        ca_file: None,
        allow_plaintext_loopback: false,
    }
}

#[test]
fn parses_every_verb() {
    let Action::Start {
        source: s,
        limit,
        json,
    } = parse(
        "import start --source magnetico --path /db.sqlite3 --allow-share //nas/x --limit 10 --json",
    )
    else {
        panic!()
    };
    assert_eq!(s.source, SourceKind::Magnetico);
    assert_eq!(s.path.as_deref(), Some(Path::new("/db.sqlite3")));
    assert_eq!(s.allow_shares, [PathBuf::from("//nas/x")]);
    assert_eq!((limit, json), (Some(10), true));
    let Action::Inspect { source: s, .. } =
        parse("import inspect --source bitmagnet --host db.lan --port 5433 --password-file pw")
    else {
        panic!()
    };
    assert_eq!(
        (s.host.as_deref(), s.port, s.dbname),
        (Some("db.lan"), Some(5433), None)
    );
    assert_eq!(
        parse("import status"),
        Action::Status {
            job: None,
            json: false
        }
    );
    assert_eq!(
        parse("import status 3 --json"),
        Action::Status {
            job: Some(3),
            json: true
        }
    );
    assert_eq!(parse("import pause 3"), Action::Pause { job: 3 });
    assert_eq!(parse("import cancel 3"), Action::Cancel { job: 3 });
    assert_eq!(
        parse("import remove 3 --yes"),
        Action::Remove {
            job: 3,
            yes: true,
            json: false
        }
    );
    assert!(matches!(
        parse("import resume 3 --source magnetico --path x"),
        Action::Resume { job: 3, .. }
    ));
    // No password on the command line, and a source is required.
    for bad in [
        "import start --source bitmagnet --password p",
        "import start --path x",
        "import start --source sqlite --path x",
        "import start --source bitmagnet --ca-file a --allow-plaintext-loopback",
    ] {
        assert!(
            Cli::try_parse_from(std::iter::once("lodestar").chain(bad.split_whitespace())).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn each_source_takes_only_its_own_flags() {
    let mut m = source(SourceKind::Magnetico);
    assert!(m.check().is_err(), "a path is required");
    m.path = Some("db".into());
    assert!(m.check().is_ok());
    m.host = Some("x".into());
    let Err(Failure::Failed(e)) = m.check() else {
        panic!()
    };
    assert!(e.contains("--host"), "{e}");
    let mut b = source(SourceKind::Bitmagnet);
    assert!(b.check().is_ok());
    b.allow_shares.push("//nas/x".into());
    assert!(b.check().is_err());
}

const NOW: i64 = 1_700_000_000;

/// A Magnetico database with `n` torrents of one file each, in
/// magneticod's v2 layout.
fn magnetico_db(path: &Path, n: u32) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE torrents (
             id INTEGER PRIMARY KEY, info_hash BLOB NOT NULL UNIQUE, name TEXT NOT NULL,
             total_size INTEGER NOT NULL, discovered_on INTEGER NOT NULL,
             updated_on INTEGER, n_seeders INTEGER, n_leechers INTEGER);
         CREATE TABLE files (
             id INTEGER PRIMARY KEY, torrent_id INTEGER REFERENCES torrents ON DELETE CASCADE,
             size INTEGER NOT NULL, path TEXT NOT NULL, is_readme INTEGER, content TEXT);
         CREATE INDEX file_info_hash_index ON files (torrent_id);
         PRAGMA user_version = 2;",
    )
    .unwrap();
    for i in 1..=n {
        let mut hash = [0u8; 20];
        hash[..4].copy_from_slice(&i.to_be_bytes());
        hash[19] = 7;
        conn.execute(
            "INSERT INTO torrents (id, info_hash, name, total_size, discovered_on)
             VALUES (?1, ?2, ?3, 10, ?4)",
            params![i, &hash[..], format!("Imported torrent {i}"), NOW - 100],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (torrent_id, size, path) VALUES (?1, 10, 'a/b.bin')",
            [i],
        )
        .unwrap();
    }
}

/// A data folder with a store Lodestar has set up.
fn data(dir: &Path) -> Config {
    std::fs::create_dir_all(dir).unwrap();
    let config = Config {
        data_dir: dir.to_path_buf(),
        ..Config::default()
    };
    drop(Store::open(config.db_path(), config.store_config()).unwrap());
    config
}

fn magnetico(path: &Path) -> SourceArgs {
    SourceArgs {
        path: Some(path.to_path_buf()),
        ..source(SourceKind::Magnetico)
    }
}

fn job(config: &Config, id: i64) -> ImportJob {
    Store::open(config.db_path(), StoreConfig::default())
        .unwrap()
        .import_job(id)
        .unwrap()
        .unwrap()
}

#[test]
fn a_magnetico_pilot_runs_pauses_resumes_and_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("database.sqlite3");
    magnetico_db(&src, 700);
    let config = data(&dir.path().join("data"));
    let args = magnetico(&src);

    block_on(inspect(&config, &args, true)).unwrap();
    // The pilot: one batch, then paused.
    block_on(start(&config, &args, Some(1), true, None)).unwrap();
    let j = job(&config, 1);
    assert_eq!((j.control, j.counters.read), (Control::Paused, 500));
    // The same source again is the same job.
    assert!(matches!(
        block_on(start(&config, &args, None, true, None)),
        Err(Failure::Refused(m)) if m.contains("job 1")
    ));
    status(&config, None, true).unwrap();
    block_on(resume(&config, 1, &args, None, true)).unwrap();
    let j = job(&config, 1);
    assert_eq!(
        (j.phase, j.counters.read, j.counters.new),
        (Phase::Indexing, 700, 700)
    );
    assert!(j.counters.conserved());

    // Rolled back: cancelled, then removed.
    run(
        &config,
        ImportArgs {
            action: Action::Cancel { job: 1 },
        },
    )
    .unwrap();
    remove(&config, 1, true, true).unwrap();
    let store = Store::open(config.db_path(), StoreConfig::default()).unwrap();
    assert_eq!(store.count().unwrap(), 0);
    assert_eq!(
        store.import_job(1).unwrap().unwrap().counters.new,
        700,
        "history stays"
    );
}

#[test]
fn writing_verbs_refuse_while_the_store_is_open_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("database.sqlite3");
    magnetico_db(&src, 3);
    let config = data(&dir.path().join("data"));
    let server = Store::open(config.db_path(), config.store_config()).unwrap();
    server.count().unwrap();
    let r = block_on(start(&config, &magnetico(&src), None, true, None));
    assert!(
        matches!(&r, Err(Failure::Refused(m)) if m.contains("stopped")),
        "{r:?}"
    );
    // Reading is fine meanwhile.
    block_on(inspect(&config, &magnetico(&src), true)).unwrap();
    status(&config, None, false).unwrap();
    drop(server);
    block_on(start(&config, &magnetico(&src), None, true, None)).unwrap();

    // No store yet: Lodestar sets it up first.
    let fresh = Config {
        data_dir: dir.path().join("fresh"),
        ..Config::default()
    };
    assert!(matches!(
        block_on(start(&fresh, &magnetico(&src), None, true, None)),
        Err(Failure::Refused(m)) if m.contains("Start Lodestar once")
    ));
}

#[test]
fn a_missing_or_refused_source_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = data(&dir.path().join("data"));
    let missing = magnetico(&dir.path().join("nope.sqlite3"));
    assert!(matches!(
        block_on(start(&config, &missing, None, true, None)),
        Err(Failure::Refused(m)) if m.contains("[")
    ));
    let not_a_db = dir.path().join("notes.txt");
    std::fs::write(&not_a_db, "hello").unwrap();
    assert!(block_on(inspect(&config, &magnetico(&not_a_db), true)).is_err());
    let store = Store::open(config.db_path(), StoreConfig::default()).unwrap();
    assert!(store.import_jobs(None, 10).unwrap().is_empty());
}

#[test]
fn a_legacy_cursor_is_bound_once_to_its_own_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let config = data(&dir.path().join("data"));
    let mut store = open_exclusive(&config).unwrap();
    let dataset = [5u8; 32];
    let job = store
        .create_import_job(&NewImportJob {
            adapter: bitmagnet::ADAPTER,
            adapter_ver: 1,
            dataset: &dataset,
            schema_sig: &[6; 32],
            options: b"",
            credential: None,
            at: NOW,
        })
        .unwrap();
    let cursor = dir.path().join("import-bitmagnet.cursor");
    let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let position = bitmagnet::position(&[9; 20]);
    // Another database's cursor is refused, and stays where it is.
    std::fs::write(
        &cursor,
        format!("bitmagnet-cursor-v2 {} {}\n", hex(&[1; 32]), hex(&position)),
    )
    .unwrap();
    assert!(matches!(
        bind_legacy_cursor(&mut store, &job, &cursor, &dataset),
        Err(Failure::Refused(_))
    ));
    assert!(cursor.exists());
    std::fs::write(
        &cursor,
        format!("bitmagnet-cursor-v2 {} {}\n", hex(&dataset), hex(&position)),
    )
    .unwrap();
    bind_legacy_cursor(&mut store, &job, &cursor, &dataset).unwrap();
    let j = store.import_job(job.id).unwrap().unwrap();
    assert_eq!((j.seq, &j.position, j.counters.read), (1, &position, 0));
    assert!(!cursor.exists(), "set aside, so it's bound once");
    // No file: nothing to bind.
    bind_legacy_cursor(&mut store, &j, &cursor, &dataset).unwrap();
}

#[test]
fn space_is_estimated_per_torrent_and_refused_when_short() {
    let s = Space {
        per_record: 4096,
        measured: false,
        needed: Some(10),
        free: Some(5),
    };
    assert!(s.short());
    assert!(
        !Space { free: None, ..s }.short(),
        "unknown free space isn't refused"
    );
    let dir = tempfile::tempdir().unwrap();
    let config = data(dir.path());
    let store = Store::open(config.db_path(), StoreConfig::default()).unwrap();
    let s = space(&config, &store, Some(1_000));
    assert_eq!((s.per_record, s.measured), (DEFAULT_RECORD_BYTES, false));
    assert_eq!(s.needed, Some(1_000 * DEFAULT_RECORD_BYTES * 125 / 100));
}
