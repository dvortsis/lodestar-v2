//! The adapter against a real PostgreSQL 16: one database per fixture,
//! created `SQL_ASCII` as IMP-0's restore is. They run when
//! `LODESTAR_TEST_POSTGRES` names a server the tests may create databases
//! on. CI's importer job sets `LODESTAR_REQUIRE_POSTGRES` too, so there a
//! missing or unreachable server fails the tests instead of skipping them.

use std::str::FromStr;
use std::time::{Duration, Instant};

use lodestar_store::import::{
    BatchCommitted, Control, ImportBatch, ImportJob, ImportRecord, NewImportJob, Phase,
    Revalidation,
};
use lodestar_store::{Store, StoreConfig};
use tokio_postgres::{Client, NoTls};

use super::*;

const NOW: i64 = 1_700_000_000;
const PASS: Decision<'static> = Decision {
    hits: &[],
    marks: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// The observed 0.10.0 schema: `files_status`, `files_count`,
    /// `torrent_pieces`.
    V0_10,
    /// 0.7–0.9: `torrent_pieces`, no `files_count`.
    NoFilesCount,
    /// 0.10.0's columns without `torrent_pieces`: no piece data at all.
    NoPieces,
    /// 0.2–0.6: piece columns on `torrents`, no `files_count`.
    PieceColumns,
    /// The first schema: `single_file`, piece columns on `torrents`.
    Legacy,
}

fn ddl(layout: Layout) -> String {
    let status = match layout {
        Layout::Legacy => "single_file boolean,",
        _ => "files_status \"FilesStatus\" NOT NULL DEFAULT 'no_info',",
    };
    let count = match layout {
        Layout::V0_10 | Layout::NoPieces => "files_count integer,",
        _ => "",
    };
    let piece_cols = match layout {
        Layout::PieceColumns | Layout::Legacy => "piece_length bigint, pieces bytea,",
        _ => "",
    };
    let pieces_table = match layout {
        Layout::V0_10 | Layout::NoFilesCount => {
            "CREATE TABLE torrent_pieces (
                info_hash bytea NOT NULL PRIMARY KEY REFERENCES torrents ON DELETE CASCADE,
                piece_length bigint NOT NULL,
                pieces bytea NOT NULL,
                created_at timestamptz NOT NULL);"
        }
        _ => "",
    };
    format!(
        "CREATE TYPE \"FilesStatus\" AS ENUM ('no_info', 'single', 'multi', 'over_threshold');
         CREATE TABLE torrents (
            info_hash bytea NOT NULL PRIMARY KEY,
            name text NOT NULL,
            size bigint NOT NULL,
            private boolean NOT NULL,
            {status} {piece_cols} {count}
            created_at timestamptz NOT NULL,
            updated_at timestamptz NOT NULL);
         CREATE TABLE torrent_files (
            info_hash bytea NOT NULL REFERENCES torrents ON DELETE CASCADE,
            \"index\" integer NOT NULL,
            path text NOT NULL,
            size bigint NOT NULL,
            created_at timestamptz NOT NULL,
            updated_at timestamptz NOT NULL,
            PRIMARY KEY (info_hash, path),
            UNIQUE (info_hash, \"index\"));
         {pieces_table}"
    )
}

/// One test at a time: the quiescence check watches the cluster's next
/// transaction id, which another test's fixture writes would move.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The admin connection string and this test's turn, or `None` to skip.
/// Panics where CI requires PostgreSQL.
async fn base() -> Option<(String, tokio::sync::MutexGuard<'static, ()>)> {
    let base = std::env::var("LODESTAR_TEST_POSTGRES").ok();
    if base.is_none() {
        assert!(
            std::env::var_os("LODESTAR_REQUIRE_POSTGRES").is_none(),
            "LODESTAR_REQUIRE_POSTGRES is set but LODESTAR_TEST_POSTGRES isn't"
        );
        eprintln!("skipped: set LODESTAR_TEST_POSTGRES to run");
    }
    Some((base?, SERIAL.lock().await))
}

async fn admin(base: &str, dbname: Option<&str>) -> Client {
    let mut cfg = tokio_postgres::Config::from_str(base).unwrap();
    if let Some(d) = dbname {
        cfg.dbname(d);
    }
    let (client, conn) = cfg
        .connect(NoTls)
        .await
        .expect("LODESTAR_TEST_POSTGRES must be reachable");
    tokio::spawn(conn);
    client
}

/// A fresh `SQL_ASCII` database, dropped first if a crashed run left it.
struct Fixture {
    base: String,
    name: String,
    db: Client,
}

impl Fixture {
    async fn new(base: &str, tag: &str, layout: Option<Layout>) -> Fixture {
        let name = format!("lodestar_imp3_{}_{tag}", std::process::id());
        let a = admin(base, None).await;
        a.batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .await
            .unwrap();
        a.batch_execute(&format!(
            "CREATE DATABASE {name} ENCODING 'SQL_ASCII' LC_COLLATE 'C' LC_CTYPE 'C' \
             TEMPLATE template0"
        ))
        .await
        .unwrap();
        let db = admin(base, Some(&name)).await;
        if let Some(l) = layout {
            db.batch_execute(&ddl(l)).await.unwrap();
        }
        Fixture {
            base: base.into(),
            name,
            db,
        }
    }

    fn settings(&self) -> Settings {
        let cfg = tokio_postgres::Config::from_str(&self.base).unwrap();
        let host = match &cfg.get_hosts()[0] {
            tokio_postgres::config::Host::Tcp(h) => h.clone(),
            #[cfg(unix)]
            tokio_postgres::config::Host::Unix(p) => p.to_string_lossy().into_owned(),
        };
        let mut s = Settings::new(&host, &self.name, cfg.get_user().unwrap_or("postgres"));
        s.port = cfg.get_ports().first().copied().unwrap_or(5432);
        s.tls = Tls::PlaintextLoopback;
        s
    }

    /// Flushes the fixture writer's table statistics, which PostgreSQL
    /// otherwise sends up to 10 s later: a late flush would read as a
    /// write mid-import. The flush runs as the session goes idle after the
    /// request, so the second statement returns after it.
    async fn settle(&self) {
        self.db
            .batch_execute("SELECT pg_stat_force_next_flush()")
            .await
            .unwrap();
        self.db.batch_execute("SELECT 1").await.unwrap();
    }

    async fn open(&self) -> Source {
        self.settle().await;
        Source::open(&self.settings()).await.unwrap()
    }

    async fn drop_db(self) {
        drop(self.db);
        admin(&self.base, None)
            .await
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.name
            ))
            .await
            .unwrap();
    }
}

/// One torrent row with its files.
struct Row {
    key: Vec<u8>,
    name: Vec<u8>,
    size: i64,
    private: bool,
    status: &'static str,
    files_count: Option<i32>,
    files: Vec<(i32, Vec<u8>, i64)>,
}

fn row(n: u8, status: &'static str, files: &[(&str, i64)]) -> Row {
    Row {
        key: vec![n; 20],
        name: format!("Torrent.{n}").into_bytes(),
        size: files.iter().map(|f| f.1).sum::<i64>().max(1),
        private: false,
        status,
        files_count: (status == "multi").then_some(files.len() as i32),
        files: files
            .iter()
            .enumerate()
            .map(|(i, (p, s))| (i as i32, p.as_bytes().to_vec(), *s))
            .collect(),
    }
}

async fn insert(db: &Client, layout: Layout, rows: &[Row]) {
    let (status_col, status_val) = match layout {
        Layout::Legacy => (
            "single_file",
            "CASE $5 WHEN 'single' THEN true WHEN 'multi' THEN false END",
        ),
        _ => ("files_status", "$5::text::\"FilesStatus\""),
    };
    let count = if matches!(layout, Layout::V0_10 | Layout::NoPieces) {
        ", files_count"
    } else {
        ""
    };
    let count_val = if count.is_empty() { "" } else { ", $6" };
    let pieces = if matches!(layout, Layout::PieceColumns | Layout::Legacy) {
        ", piece_length, pieces"
    } else {
        ""
    };
    let pieces_val = if pieces.is_empty() {
        ""
    } else {
        ", 262144, '\\x0000000000000000000000000000000000000000'::bytea"
    };
    let sql = format!(
        "INSERT INTO torrents (info_hash, name, size, private, {status_col}{count}{pieces}, \
                               created_at, updated_at) \
         VALUES ($1, convert_from($2, 'SQL_ASCII'), $3, $4, {status_val}{count_val}{pieces_val}, \
                 to_timestamp(1600000000), now())"
    );
    let stmt = db.prepare(&sql).await.unwrap();
    for r in rows {
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            vec![&r.key, &r.name, &r.size, &r.private, &r.status];
        if !count.is_empty() {
            params.push(&r.files_count);
        }
        db.execute(&stmt, &params).await.unwrap();
        for (i, path, size) in &r.files {
            db.execute(
                "INSERT INTO torrent_files VALUES ($1, $2, convert_from($3, 'SQL_ASCII'), $4, \
                 now(), now())",
                &[&r.key, i, path, size],
            )
            .await
            .unwrap();
        }
    }
    if matches!(layout, Layout::V0_10 | Layout::NoFilesCount) {
        db.batch_execute(
            "INSERT INTO torrent_pieces SELECT info_hash, 262144, \
             '\\x0000000000000000000000000000000000000000'::bytea, now() FROM torrents",
        )
        .await
        .unwrap();
    }
}

/// `n` multi-file torrents, keys 1…n big-endian, one 1-byte file each.
async fn many(db: &Client, n: i32) {
    db.batch_execute(&format!(
        "INSERT INTO torrents (info_hash, name, size, private, files_status, files_count, \
                               created_at, updated_at) \
         SELECT decode(lpad(to_hex(g), 40, '0'), 'hex'), 'T' || g, 1, false, 'multi', 1, \
                now(), now() FROM generate_series(1, {n}) g; \
         INSERT INTO torrent_files SELECT info_hash, 0, 'f', 1, now(), now() FROM torrents; \
         INSERT INTO torrent_pieces SELECT info_hash, 262144, \
                '\\x0000000000000000000000000000000000000000'::bytea, now() FROM torrents"
    ))
    .await
    .unwrap();
}

async fn all(src: &mut Source) -> Vec<Batch> {
    all_from(src, Vec::new()).await
}

async fn all_from(src: &mut Source, mut pos: Vec<u8>) -> Vec<Batch> {
    let mut out = Vec::new();
    while let Some(b) = src.read_batch(&pos, NOW).await.unwrap() {
        pos = b.position.clone();
        out.push(b);
    }
    out
}

fn torrents(batches: &[Batch]) -> Vec<&TorrentRecord> {
    batches
        .iter()
        .flat_map(|b| &b.records)
        .filter_map(|r| match r {
            Record::Torrent(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_layout_is_read() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    for (tag, layout, caps) in [
        ("v010", Layout::V0_10, (true, true, Pieces::Table)),
        (
            "nocount",
            Layout::NoFilesCount,
            (true, false, Pieces::Table),
        ),
        ("nopieces", Layout::NoPieces, (true, true, Pieces::None)),
        (
            "piececols",
            Layout::PieceColumns,
            (true, false, Pieces::Columns),
        ),
        ("legacy", Layout::Legacy, (false, false, Pieces::Columns)),
    ] {
        let fx = Fixture::new(&base, tag, Some(layout)).await;
        insert(
            &fx.db,
            layout,
            &[
                row(1, "single", &[]),
                row(2, "multi", &[("E01.mkv", 1000), ("Subs/en.srt", 2000)]),
                row(3, "no_info", &[]),
            ],
        )
        .await;
        let mut src = fx.open().await;
        let c = src.caps();
        assert_eq!(
            (c.files_status, c.files_count, c.pieces),
            caps,
            "{layout:?}"
        );
        let batches = all(&mut src).await;
        let t = torrents(&batches);
        assert_eq!(t.len(), 3, "{layout:?}");
        assert!(t[0].single_file && t[0].completeness == Completeness::ReportedComplete);
        assert_eq!(
            t[1].completeness,
            Completeness::ReportedComplete,
            "{layout:?}"
        );
        assert_eq!(
            t[1].files[1].path,
            vec![b"Subs".to_vec(), b"en.srt".to_vec()]
        );
        assert_eq!(t[2].repair, repair::MISSING_FILES);
        let pieces = if caps.2 == Pieces::None { 0 } else { 1 };
        assert_eq!(t[0].piece_count, pieces, "{layout:?}");
        // A second open of the same database is the same source.
        drop(src);
        let again = fx.open().await;
        assert_eq!(again.identity(), fx.open().await.identity());
        drop(again);
        fx.drop_db().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_schemas_are_refused_by_name() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "empty", None).await;
    let Err(Refusal::UnsupportedSchema(missing)) = Source::open(&fx.settings()).await else {
        panic!("an empty database opened")
    };
    assert!(missing.contains(&"torrents.info_hash") && missing.contains(&"torrent_files.path"));
    fx.drop_db().await;

    let fx = Fixture::new(&base, "nostatus", Some(Layout::NoPieces)).await;
    fx.db
        .batch_execute("ALTER TABLE torrents DROP COLUMN files_status")
        .await
        .unwrap();
    assert_eq!(
        Source::open(&fx.settings()).await.unwrap_err(),
        Refusal::UnsupportedSchema(vec!["torrents.files_status"])
    );
    fx.drop_db().await;

    let fx = Fixture::new(&base, "noindex", Some(Layout::NoPieces)).await;
    fx.db
        .batch_execute(
            "ALTER TABLE torrent_files DROP CONSTRAINT torrent_files_info_hash_index_key; \
             ALTER TABLE torrent_files DROP CONSTRAINT torrent_files_pkey",
        )
        .await
        .unwrap();
    assert_eq!(
        Source::open(&fx.settings()).await.unwrap_err(),
        Refusal::UnsupportedSchema(vec!["index: torrent_files(info_hash, index)"])
    );
    fx.drop_db().await;

    // A cast on the source's own enum is refused before a row is read: an
    // implicit one's function would run as our role.
    let fx = Fixture::new(&base, "casts", Some(Layout::V0_10)).await;
    for (target, value) in [("text", "'x'"), ("bool", "true")] {
        fx.db
            .batch_execute(&format!(
                "CREATE FUNCTION public.f(\"FilesStatus\") RETURNS {target} \
                 LANGUAGE sql AS 'SELECT {}'; \
                 CREATE CAST (\"FilesStatus\" AS {target}) WITH FUNCTION \
                 public.f(\"FilesStatus\") AS IMPLICIT",
                value.replace('\'', "''")
            ))
            .await
            .unwrap();
        assert_eq!(
            Source::open(&fx.settings()).await.unwrap_err(),
            Refusal::UnsupportedSchema(vec!["a cast on torrents.files_status (refused)"]),
            "{target}"
        );
        fx.db
            .batch_execute("DROP FUNCTION public.f(\"FilesStatus\") CASCADE")
            .await
            .unwrap();
    }
    fx.drop_db().await;

    // A keyset index with the source's own operator class doesn't count.
    let fx = Fixture::new(&base, "opclass", Some(Layout::NoPieces)).await;
    fx.db
        .batch_execute(
            "CREATE OPERATOR CLASS public.my_bytea_ops FOR TYPE bytea USING btree AS \
               OPERATOR 1 <, OPERATOR 2 <=, OPERATOR 3 =, OPERATOR 4 >=, OPERATOR 5 >, \
               FUNCTION 1 byteacmp(bytea, bytea); \
             ALTER TABLE torrents DROP CONSTRAINT torrents_pkey CASCADE; \
             CREATE UNIQUE INDEX ON torrents (info_hash public.my_bytea_ops)",
        )
        .await
        .unwrap();
    assert_eq!(
        Source::open(&fx.settings()).await.unwrap_err(),
        Refusal::UnsupportedSchema(vec!["unique_index: torrents.info_hash"])
    );
    fx.drop_db().await;

    // A type of the source's own that's named like a built-in doesn't pass.
    let fx = Fixture::new(&base, "domain", Some(Layout::NoPieces)).await;
    fx.db
        .batch_execute(
            "CREATE DOMAIN public.text AS pg_catalog.text; \
             ALTER TABLE torrents ALTER COLUMN name TYPE public.text",
        )
        .await
        .unwrap();
    assert_eq!(
        Source::open(&fx.settings()).await.unwrap_err(),
        Refusal::UnsupportedSchema(vec!["torrents.name"])
    );
    fx.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_is_read_only_and_bounded() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "session", Some(Layout::V0_10)).await;
    insert(&fx.db, Layout::V0_10, &[row(1, "single", &[])]).await;
    let mut s = fx.settings();
    s.statement_timeout = Duration::from_secs(2);
    fx.settle().await;
    let src = Source::open(&s).await.unwrap();
    let c = src.client();
    for (name, want) in [
        ("default_transaction_read_only", "on"),
        ("statement_timeout", "2s"),
        ("lock_timeout", "10s"),
        ("idle_in_transaction_session_timeout", "62s"),
        ("search_path", "pg_catalog"),
        ("row_security", "off"),
    ] {
        let got: String = c
            .query_one(&format!("SHOW {name}"), &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(got, want, "{name}");
    }
    for write in [
        "INSERT INTO public.torrents SELECT * FROM public.torrents",
        "DELETE FROM public.torrent_files",
        "CREATE TABLE public.x (a int)",
        "CREATE TEMP TABLE x (a int)",
    ] {
        let e = c.batch_execute(write).await.unwrap_err();
        assert_eq!(e.as_db_error().unwrap().code().code(), "25006", "{write}");
    }
    let e = c.query("SELECT pg_sleep(10)", &[]).await.unwrap_err();
    assert_eq!(map_err(e), Refusal::StatementTimeout);
    drop(src);
    fx.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_is_required_and_verified() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "tls", Some(Layout::V0_10)).await;
    // CI's server has no TLS: asking for it fails, never falls back.
    let mut s = fx.settings();
    s.tls = Tls::Verify { ca_file: None };
    if !s.host.starts_with('/') {
        assert_eq!(
            Source::open(&s).await.unwrap_err(),
            Refusal::Unavailable("tls_failed")
        );
    }
    // Plaintext off loopback is refused before connecting.
    let mut s = fx.settings();
    s.host = "192.0.2.1".into();
    assert_eq!(Source::open(&s).await.unwrap_err(), Refusal::TlsRequired);
    fx.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_bytes_and_nameless_rows_survive() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "bytes", Some(Layout::V0_10)).await;
    let mut odd = row(1, "multi", &[]);
    odd.name = vec![b'G', 0xb5, 0xc4, 0xff];
    odd.size = 5;
    odd.files_count = Some(1);
    odd.files = vec![(0, vec![b'd', 0xfe, b'/', 0xe9, b'f'], 5)];
    let mut nameless = row(2, "single", &[]);
    nameless.name = Vec::new();
    nameless.private = true;
    let mut zero = row(3, "single", &[]);
    zero.size = 0;
    insert(&fx.db, Layout::V0_10, &[odd, nameless, zero]).await;
    fx.db
        .batch_execute(
            "UPDATE torrents SET created_at = 'infinity' WHERE info_hash = '\\x0101010101010101010101010101010101010101'",
        )
        .await
        .unwrap();
    let mut src = fx.open().await;
    let batches = all(&mut src).await;
    let b = &batches[0];
    let Record::Torrent(t) = &b.records[0] else {
        panic!()
    };
    assert_eq!(t.name, vec![b'G', 0xb5, 0xc4, 0xff]);
    assert_eq!(t.files[0].path, vec![vec![b'd', 0xfe], vec![0xe9, b'f']]);
    assert_eq!(t.source_at, None);
    assert_eq!(b.findings.get(Finding::NameLossy), 1);
    assert_eq!(b.findings.get(Finding::PathLossy), 1);
    assert_eq!(b.findings.get(Finding::DateOutOfRange), 1);
    let Record::Candidate(c) = &b.records[1] else {
        panic!()
    };
    assert!(c.name.is_none() && c.private && c.size.is_some());
    let Record::Candidate(c) = &b.records[2] else {
        panic!()
    };
    assert!(c.name.is_some() && c.size.is_none());

    // Into a store: the original bytes kept in name_raw, candidates kept.
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let job = reading_job(&mut store, &src.identity());
    assert_eq!(commit(&mut store, job.id, 1, b), "committed");
    let id = store.find(&InfoHash([1; 20])).unwrap().unwrap();
    let stored = store.torrent(id).unwrap().unwrap();
    assert_eq!(stored.name_raw.unwrap().raw, vec![b'G', 0xb5, 0xc4, 0xff]);
    let job = store.import_job(job.id).unwrap().unwrap();
    assert_eq!(
        (job.counters.read, job.counters.new, job.counters.candidates),
        (3, 1, 2)
    );
    drop(src);
    fx.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batches_are_bounded_by_count_and_memory() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "bounds", Some(Layout::V0_10)).await;
    many(&fx.db, 1_100).await;
    let mut src = fx.open().await;
    let sizes: Vec<usize> = all(&mut src)
        .await
        .iter()
        .map(|b| b.records.len())
        .collect();
    assert_eq!(sizes, [500, 500, 100]);
    drop(src);

    // A parent whose files are over the 8 MiB budget, between two others:
    // it's kept as a partial prefix, and the next parent's files are still
    // read after the reader skips the rest of its rows.
    let path = "p".repeat(1000) + "/f";
    let huge = "decode(lpad(to_hex(1024), 40, '0'), 'hex')";
    fx.db
        .batch_execute(&format!(
            "DELETE FROM torrents WHERE info_hash = {huge}; \
             INSERT INTO torrents (info_hash, name, size, private, files_status, files_count, \
                                   created_at, updated_at) \
             VALUES ({huge}, 'Huge', 10000, false, 'multi', 10000, now(), now()); \
             INSERT INTO torrent_files SELECT {huge}, g, '{path}' || g, 1, now(), now() \
             FROM generate_series(0, 9999) g"
        ))
        .await
        .unwrap();
    let key = |n: u16| [vec![0; 18], n.to_be_bytes().to_vec()].concat();
    let mut src = fx.open().await;
    src.set_max_parents(3);
    let b = src
        .read_batch(&position(&key(1022)), NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.records.len(), 3);
    let Record::Torrent(t) = &b.records[1] else {
        panic!()
    };
    assert_eq!(t.completeness, Completeness::Partial);
    assert!(
        t.files.len() > 100 && t.files.len() < 10_000,
        "{}",
        t.files.len()
    );
    assert!(t.files_dropped >= 1);
    assert_eq!(b.findings.get(Finding::FilesOverLimit), 1);
    let kept: usize = t
        .files
        .iter()
        .map(|f| f.path.concat().len() + 64 + 24 * f.path.len())
        .sum();
    assert!(kept <= MAX_BATCH_BYTES);
    // Its files come in index order, as a prefix.
    assert_eq!(t.files[1].path.last().unwrap(), b"f1");
    // The parent after it still gets its files.
    for r in [&b.records[0], &b.records[2]] {
        let Record::Torrent(next) = r else { panic!() };
        assert_eq!(next.completeness, Completeness::ReportedComplete);
    }
    let rest = all_from(&mut src, b.position.clone()).await;
    assert_eq!(
        rest.iter().map(|b| b.records.len()).sum::<usize>() + b.records.len(),
        1_100 - 1022
    );
    drop(src);
    fx.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_source_is_refused() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "live", Some(Layout::V0_10)).await;
    many(&fx.db, 10).await;
    // A writer mid-transaction: refused at open.
    let mut writer = admin(&base, Some(&fx.name)).await;
    let tx = writer.transaction().await.unwrap();
    tx.batch_execute("UPDATE torrents SET size = 2 WHERE false")
        .await
        .unwrap();
    assert_eq!(
        Source::open(&fx.settings()).await.unwrap_err(),
        Refusal::NotQuiescent
    );
    tx.rollback().await.unwrap();

    // A write committed after the open: the next batch refuses.
    let mut src = fx.open().await;
    let b = src.read_batch(&[], NOW).await.unwrap().unwrap();
    let before = write_count(&fx.db).await;
    writer
        .batch_execute(
            "DELETE FROM torrents WHERE info_hash = '\\x000000000000000000000000000000000000000a'",
        )
        .await
        .unwrap();
    writer
        .batch_execute("SELECT pg_stat_force_next_flush()")
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while write_count(&fx.db).await == before {
        assert!(
            Instant::now() < deadline,
            "the write never reached the statistics"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        src.read_batch(&b.position, NOW).await.unwrap_err(),
        Refusal::NotQuiescent
    );
    drop(src);
    drop(writer);
    fx.drop_db().await;
}

async fn write_count(db: &Client) -> i64 {
    db.query_one(
        "SELECT pg_stat_get_tuples_deleted('torrents'::regclass)",
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resumed_job_refuses_another_database_with_the_same_name() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "resume", Some(Layout::V0_10)).await;
    many(&fx.db, 600).await;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let mut src = fx.open().await;
    let job = reading_job(&mut store, &src.identity());
    let b1 = src.read_batch(&[], NOW).await.unwrap().unwrap();
    commit(&mut store, job.id, 1, &b1);
    let old_fingerprint = src.fingerprint;
    // A replay writes nothing.
    assert_eq!(commit(&mut store, job.id, 1, &b1), "replayed");
    drop(src);
    assert_eq!(store.interrupt_import_jobs(NOW).unwrap(), 1);

    // The same name, the same rows, but another database.
    let settings = fx.settings();
    fx.drop_db().await;
    let fx = Fixture::new(&base, "resume", Some(Layout::V0_10)).await;
    many(&fx.db, 600).await;
    assert_eq!(fx.settings().dbname, settings.dbname);
    let fresh = fx.open().await;
    // The binding is the database's and tables' OIDs, not the content.
    assert_ne!(fresh.fingerprint, old_fingerprint);
    let id = fresh.identity();
    drop(fresh);
    let job = store.import_job(job.id).unwrap().unwrap();
    let job = store
        .resume_import_job(
            job.id,
            job.revision,
            &Revalidation {
                dataset: &id.dataset,
                schema_sig: &id.schema_sig,
                credential_ok: true,
                space_ok: true,
            },
            NOW,
        )
        .unwrap();
    assert_eq!(job.control, Control::Blocked);
    assert_eq!(job.reason, Some(Reason::SourceChanged));
    fx.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_legacy_import_runs_on_the_adapter() {
    let Some((base, _serial)) = base().await else {
        return;
    };
    let fx = Fixture::new(&base, "legacyimport", Some(Layout::V0_10)).await;
    let mut private = row(3, "over_threshold", &[]);
    private.private = true;
    let mut bad = row(4, "single", &[]);
    bad.key = vec![9; 32];
    insert(
        &fx.db,
        Layout::V0_10,
        &[
            row(1, "single", &[]),
            row(2, "multi", &[("a", 1000), ("b/c", 2000)]),
            private,
            bad,
        ],
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("l.db"), StoreConfig::default()).unwrap();
    let source = store.register_source(PLUGIN_ID, "1", 0).unwrap();
    let opts = Options {
        batch: 2,
        cursor: Some(dir.path().join("cursor")),
        limit: None,
    };
    let mut batches = 0;
    fx.settle().await;
    let report = import(&fx.settings(), &mut store, source, &opts, |_| batches += 1)
        .await
        .unwrap();
    assert_eq!(
        report,
        Report {
            read: 4,
            new: 3,
            existing: 0,
            partial: 1,
            skipped: 1
        }
    );
    assert_eq!(batches, 2);
    let id = store.find(&InfoHash([3; 20])).unwrap().unwrap();
    let t = store.torrent(id).unwrap().unwrap();
    assert_ne!(t.flags & flags::PRIVATE, 0);
    assert_ne!(t.flags & flags::FILES_PARTIAL, 0);
    let again = import(&fx.settings(), &mut store, source, &opts, |_| {})
        .await
        .unwrap();
    assert_eq!(again.read, 0);
    // The cursor is bound to this database: a new one under the same name
    // is refused, not resumed past the old cursor.
    fx.drop_db().await;
    let fx = Fixture::new(&base, "legacyimport", Some(Layout::V0_10)).await;
    insert(&fx.db, Layout::V0_10, &[row(1, "single", &[])]).await;
    fx.settle().await;
    let err = import(&fx.settings(), &mut store, source, &opts, |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(err, ImportError::Source(Refusal::SourceChanged)),
        "{err}"
    );
    fx.drop_db().await;
}

fn reading_job(store: &mut Store, id: &Identity) -> ImportJob {
    let job = store
        .create_import_job(&NewImportJob {
            adapter: ADAPTER,
            adapter_ver: ADAPTER_VERSION,
            dataset: &id.dataset,
            schema_sig: &id.schema_sig,
            options: b"",
            credential: None,
            at: NOW,
        })
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Ready, NOW)
        .unwrap();
    store
        .set_import_phase(job.id, job.revision, Phase::Reading, NOW)
        .unwrap()
}

/// Commits one source batch as one IMP-1 batch, the way IMP-4 will.
fn commit(store: &mut Store, job: i64, seq: u64, b: &Batch) -> &'static str {
    let source = store.register_source(ADAPTER, "1", NOW).unwrap();
    let mut preparer = store.preparer();
    let prepared: Vec<Option<Prepared>> = b
        .records
        .iter()
        .map(|r| match r {
            Record::Torrent(t) => preparer.prepare(&t.info(), None).ok(),
            _ => None,
        })
        .collect();
    let records: Vec<ImportRecord<'_>> = b
        .records
        .iter()
        .zip(&prepared)
        .map(|(r, p)| match (r, p) {
            (Record::Torrent(t), Some(p)) => ImportRecord::Torrent(t.import(p, PASS)),
            (Record::Candidate(c), _) => ImportRecord::Candidate(c.import(PASS)),
            _ => ImportRecord::Invalid,
        })
        .collect();
    match store
        .commit_import_batch(&ImportBatch {
            job,
            seq,
            position: &b.position,
            source,
            source_bytes: b.source_bytes,
            at: NOW,
            class_gen: None,
            records: &records,
        })
        .unwrap()
    {
        BatchCommitted::Committed { .. } => "committed",
        BatchCommitted::Replayed { .. } => "replayed",
    }
}
