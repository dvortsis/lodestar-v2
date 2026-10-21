//! A bitmagnet-shaped PostgreSQL schema, imported for real. Runs when
//! `LODESTAR_TEST_POSTGRES` points at a server the test may create a
//! scratch schema on (CI provides one); skipped otherwise.

use lodestar_metainfo::InfoHash;
use lodestar_sources_importers::bitmagnet::{Options, PLUGIN_ID, Report, import};
use lodestar_store::{SourceId, Store, StoreConfig, flags};
use tokio_postgres::NoTls;

const SCHEMA: &str = r#"
CREATE TYPE "FilesStatus" AS ENUM ('no_info', 'single', 'multi', 'over_threshold');
CREATE TABLE torrents (
    info_hash bytea PRIMARY KEY,
    name text NOT NULL,
    size bigint NOT NULL,
    private boolean NOT NULL,
    files_status "FilesStatus" NOT NULL DEFAULT 'no_info',
    files_count integer,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE TABLE torrent_files (
    info_hash bytea NOT NULL REFERENCES torrents ON DELETE CASCADE,
    "index" integer NOT NULL,
    path text NOT NULL,
    size bigint NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (info_hash, path)
);
CREATE TABLE torrent_pieces (
    info_hash bytea PRIMARY KEY REFERENCES torrents ON DELETE CASCADE,
    piece_length bigint NOT NULL,
    pieces bytea NOT NULL,
    created_at timestamptz NOT NULL
);
"#;

/// info_hash, name, size, private, files_status, files_count, created_at.
type TorrentRow = (
    Vec<u8>,
    &'static str,
    i64,
    bool,
    &'static str,
    Option<i32>,
    f64,
);

fn hash(n: u8) -> Vec<u8> {
    vec![n; 20]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imports_a_bitmagnet_database() {
    let Ok(base) = std::env::var("LODESTAR_TEST_POSTGRES") else {
        eprintln!("skipped: set LODESTAR_TEST_POSTGRES to run");
        return;
    };
    let schema = format!("lodestar_it_{}", std::process::id());
    let (admin, conn) = tokio_postgres::connect(&base, NoTls).await.unwrap();
    tokio::spawn(conn);
    admin
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema};{SCHEMA}"))
        .await
        .unwrap();
    // Single-file, multi-file, over-threshold, no-info, and a broken row.
    let t = "INSERT INTO torrents VALUES ($1, $2, $3, $4, $5::text::\"FilesStatus\", $6, to_timestamp($7), now())";
    let rows: [TorrentRow; 5] = [
        (
            hash(1),
            "Movie.2019.1080p.mkv",
            8 << 30,
            false,
            "single",
            Some(1),
            1_600_000_000.0,
        ),
        (
            hash(2),
            "Show.S01.1080p",
            3000,
            false,
            "multi",
            Some(2),
            1_600_000_100.0,
        ),
        (
            hash(3),
            "Huge.Pack",
            99 << 30,
            true,
            "over_threshold",
            Some(5000),
            1_600_000_200.0,
        ),
        (
            hash(4),
            "No.Info",
            10,
            false,
            "no_info",
            None,
            1_600_000_300.0,
        ),
        (
            vec![9; 32],
            "Bad.Hash",
            1,
            false,
            "single",
            Some(1),
            1_600_000_400.0,
        ),
    ];
    for r in &rows {
        admin
            .execute(t, &[&r.0, &r.1, &r.2, &r.3, &r.4, &r.5, &r.6])
            .await
            .unwrap();
    }
    for (i, path, size) in [(0, "Show.S01E01.mkv", 1000i64), (1, "Subs/en.srt", 2000)] {
        admin
            .execute(
                "INSERT INTO torrent_files VALUES ($1, $2, $3, $4, now(), now())",
                &[&hash(2), &i, &path, &size],
            )
            .await
            .unwrap();
    }
    admin
        .execute(
            "INSERT INTO torrent_pieces VALUES ($1, 262144, $2, now())",
            &[&hash(2), &vec![0u8; 40]],
        )
        .await
        .unwrap();
    let before: i64 = admin
        .query_one("SELECT count(*) FROM torrents", &[])
        .await
        .unwrap()
        .get(0);

    let url = format!("{base} options='-c search_path={schema}'");
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("l.db"), StoreConfig::default()).unwrap();
    let source: SourceId = store.register_source(PLUGIN_ID, "1", 0).unwrap();
    let opts = Options {
        batch: 2,
        cursor: Some(dir.path().join("cursor")),
        limit: None,
    };
    let mut batches = 0;
    let report = import(&url, &mut store, source, &opts, |_| batches += 1)
        .await
        .unwrap();
    assert_eq!(
        report,
        Report {
            read: 5,
            new: 4,
            existing: 0,
            partial: 2,
            skipped: 1
        }
    );
    assert_eq!(batches, 3, "batches of 2");

    // What landed.
    let mut row = |n: u8| {
        let id = store.find(&InfoHash([n; 20])).unwrap().unwrap();
        (
            store.torrent(id).unwrap().unwrap(),
            store.files(id).unwrap().unwrap(),
        )
    };
    let (movie, files) = row(1);
    assert_eq!(
        (movie.size, movie.discovered_at, files.files.len()),
        (8 << 30, 1_600_000_000, 1)
    );
    assert_ne!(movie.flags & flags::IMPORTED, 0);
    assert_eq!(movie.first_source, Some(source));
    let (show, files) = row(2);
    assert_eq!(
        (show.file_count, show.piece_len, show.piece_count),
        (2, 262_144, 2)
    );
    assert_eq!(
        files.files[1].path,
        vec![b"Subs".to_vec(), b"en.srt".to_vec()]
    );
    assert_eq!(show.flags & flags::FILES_PARTIAL, 0);
    let (pack, _) = row(3);
    assert_ne!(pack.flags & flags::FILES_PARTIAL, 0);
    assert_ne!(pack.flags & flags::PRIVATE, 0);
    assert_eq!(store.partial_hashes(10).unwrap().len(), 2);

    // Resuming from the cursor finds nothing new; starting over finds it
    // all stored already.
    let again = import(&url, &mut store, source, &opts, |_| {})
        .await
        .unwrap();
    assert_eq!(again.read, 0);
    let fresh = Options {
        cursor: None,
        ..opts.clone()
    };
    let over = import(&url, &mut store, source, &fresh, |_| {})
        .await
        .unwrap();
    assert_eq!((over.read, over.new, over.existing), (5, 0, 4));
    assert_eq!(store.count().unwrap(), 4);

    // Nothing written to the source.
    let after: i64 = admin
        .query_one("SELECT count(*) FROM torrents", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, after);
    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn refuses_databases_that_arent_bitmagnet() {
    let Ok(base) = std::env::var("LODESTAR_TEST_POSTGRES") else {
        return;
    };
    let schema = format!("lodestar_empty_{}", std::process::id());
    let (admin, conn) = tokio_postgres::connect(&base, NoTls).await.unwrap();
    tokio::spawn(conn);
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"
        ))
        .await
        .unwrap();
    let url = format!("{base} options='-c search_path={schema}'");
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let err = import(
        &url,
        &mut store,
        SourceId::new(0).unwrap(),
        &Options::default(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("not a bitmagnet database"),
        "{err}"
    );
    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
