use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, TextEncoding};
use lodestar_store::rusqlite::types::Value;
use lodestar_store::{Inserted, NewTorrent, Store, StoreConfig, flags};

fn d(entries: Vec<(&str, OwnedValue)>) -> OwnedValue {
    OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: impl AsRef<[u8]>) -> OwnedValue {
    OwnedValue::Bytes(s.as_ref().to_vec())
}

fn i(v: i64) -> OwnedValue {
    OwnedValue::Int(v)
}

/// A multi-file v1 info dict with `n` episode files.
fn season(name: &str, n: usize) -> Vec<u8> {
    let files = (1..=n)
        .map(|e| {
            d(vec![
                ("length", i(1_000_000 + e as i64)),
                (
                    "path",
                    OwnedValue::List(vec![b(format!("{name}.S01E{e:02}.1080p.WEB-DL.mkv"))]),
                ),
            ])
        })
        .collect();
    to_vec(&d(vec![
        ("files", OwnedValue::List(files)),
        ("name", b(name)),
        ("piece length", i(1 << 20)),
        ("pieces", b(vec![0; n * 20])),
        ("x_publisher", b("someone")),
    ]))
}

fn single(name: &[u8], extra: Vec<(&str, OwnedValue)>) -> Vec<u8> {
    let mut entries = vec![
        ("length", i(5)),
        ("name", b(name)),
        ("piece length", i(16_384)),
        ("pieces", b([0; 20])),
    ];
    entries.extend(extra);
    to_vec(&d(entries))
}

fn hybrid() -> Vec<u8> {
    let tree = d(vec![(
        "a.bin",
        d(vec![(
            "",
            d(vec![("length", i(100)), ("pieces root", b([5; 32]))]),
        )]),
    )]);
    to_vec(&d(vec![
        ("file tree", tree),
        ("length", i(100)),
        ("meta version", i(2)),
        ("name", b("a.bin")),
        ("piece length", i(16_384)),
        ("pieces", b([0; 20])),
    ]))
}

fn new<'a>(info: &'a Info, raw: &'a [u8]) -> NewTorrent<'a> {
    NewTorrent {
        info,
        raw_info: Some(raw),
        discovered_at: 1_700_000_000,
        metadata_at: 1_700_000_005,
        extra_flags: 0,
    }
}

fn mem() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

#[test]
fn file_database_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lodestar.db");
    let store = Store::open(&path, StoreConfig::default()).unwrap();
    let conn = store.connection();
    let pragma = |name: &str| -> String {
        let v: Value = conn
            .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .unwrap();
        match v {
            Value::Integer(i) => i.to_string(),
            Value::Text(t) => t,
            other => format!("{other:?}"),
        }
    };
    assert_eq!(pragma("page_size"), "16384");
    assert_eq!(pragma("journal_mode"), "wal");
    assert_eq!(pragma("auto_vacuum"), "2"); // incremental
    assert_eq!(pragma("foreign_keys"), "1");
    assert_eq!(pragma("synchronous"), "1"); // normal
    let strict: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_list WHERE schema = 'main' AND strict = 0
             AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(strict, 0, "every table is STRICT");
    drop(store);
    // Reopening applies nothing new and keeps working.
    Store::open(&path, StoreConfig::default()).unwrap();
}

#[test]
fn insert_and_read_back() {
    let mut store = mem();
    let raw = season("Some.Show", 8);
    let info = Info::parse(&raw).unwrap();
    let [Inserted::New(id)] = store.insert(&[new(&info, &raw)]).unwrap()[..] else {
        panic!("expected one new row");
    };

    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.info_hash, info.primary_hash());
    assert_eq!(row.info_hash_v2, None);
    assert_eq!(row.name, "Some.Show");
    assert_eq!(row.name_raw, None);
    assert_eq!(row.size, info.total_size);
    assert_eq!(row.piece_len, 1 << 20);
    assert_eq!(row.piece_count, 8);
    assert_eq!(row.file_count, 8);
    assert_eq!(row.info_size, raw.len() as u64);
    assert_eq!(row.flags, 0);
    assert_eq!(row.discovered_at, 1_700_000_000);
    assert_eq!(row.metadata_at, 1_700_000_005);
    assert_eq!(row.last_seen_at, 1_700_000_000);

    let tree = store.files(id).unwrap().unwrap();
    assert_eq!(tree.files, info.files);
    assert_eq!(tree.encoding, TextEncoding::UTF8);
    assert_eq!(
        store.extra(id).unwrap().as_deref(),
        Some(&b"d11:x_publisher7:someonee"[..])
    );
    // Archiving is off by default.
    assert_eq!(store.raw_info(id).unwrap(), None);
    assert_eq!(store.find(&info.primary_hash()).unwrap(), Some(id));
    assert_eq!(store.count().unwrap(), 1);
    assert!(store.torrent(id + 1).unwrap().is_none());
    assert!(store.files(id + 1).unwrap().is_none());
}

#[test]
fn duplicates_are_detected_within_and_across_batches() {
    let mut store = mem();
    let raw = season("Dup", 2);
    let info = Info::parse(&raw).unwrap();
    let out = store.insert(&[new(&info, &raw), new(&info, &raw)]).unwrap();
    assert_eq!(out, vec![Inserted::New(1), Inserted::Existing(1)]);
    assert_eq!(
        store.insert(&[new(&info, &raw)]).unwrap(),
        vec![Inserted::Existing(1)]
    );
    assert_eq!(store.count().unwrap(), 1);
}

#[test]
fn hybrid_is_found_under_both_hashes() {
    let mut store = mem();
    let raw = hybrid();
    let info = Info::parse(&raw).unwrap();
    let id = store.insert(&[new(&info, &raw)]).unwrap()[0].id();
    let v2 = info.v2.unwrap();
    assert_eq!(store.find(&info.v1.unwrap()).unwrap(), Some(id));
    assert_eq!(store.find(&v2.truncated()).unwrap(), Some(id));
    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.info_hash_v2, Some(v2));
    assert_eq!(row.flags, flags::V2 | flags::HYBRID | flags::SINGLE_FILE);
    assert_eq!(
        store.files(id).unwrap().unwrap().files[0].pieces_root,
        Some([5; 32])
    );
}

#[test]
fn legacy_names_keep_raw_bytes() {
    let mut store = mem();
    let gbk = encoding_rs::GBK
        .encode("中文电视剧第一季高清")
        .0
        .into_owned();
    let raw = single(&gbk, vec![("private", i(1))]);
    let info = Info::parse(&raw).unwrap();
    let id = store.insert(&[new(&info, &raw)]).unwrap()[0].id();
    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.name, "中文电视剧第一季高清");
    let name_raw = row.name_raw.unwrap();
    assert_eq!(name_raw.raw, gbk);
    assert_eq!(name_raw.decode(), "中文电视剧第一季高清");
    assert_eq!(row.flags, flags::PRIVATE | flags::SINGLE_FILE);
    let tree = store.files(id).unwrap().unwrap();
    assert_eq!(tree.files[0].path, vec![gbk]);
    assert_eq!(tree.encoding, info.path_encoding);
}

#[test]
fn raw_archive_strips_pieces() {
    let mut store = Store::open_in_memory(StoreConfig {
        archive_raw: true,
        ..StoreConfig::default()
    })
    .unwrap();
    let raw = single(b"a", vec![]);
    let info = Info::parse(&raw).unwrap();
    let id = store
        .insert(&[NewTorrent {
            extra_flags: flags::IMPORTED,
            ..new(&info, &raw)
        }])
        .unwrap()[0]
        .id();
    assert_eq!(
        store.raw_info(id).unwrap().as_deref(),
        Some(&b"d6:lengthi5e4:name1:a12:piece lengthi16384ee"[..])
    );
    assert_ne!(
        store.torrent(id).unwrap().unwrap().flags & flags::IMPORTED,
        0
    );
}

#[test]
fn ids_are_never_reused_and_children_cascade() {
    let mut store = mem();
    let a = single(b"a", vec![]);
    let b_ = single(b"b", vec![]);
    let info_a = Info::parse(&a).unwrap();
    let info_b = Info::parse(&b_).unwrap();
    let id_a = store.insert(&[new(&info_a, &a)]).unwrap()[0].id();
    store
        .connection()
        .execute("DELETE FROM torrents WHERE id = ?1", [id_a])
        .unwrap();
    assert!(store.files(id_a).unwrap().is_none(), "cascaded");
    let id_b = store.insert(&[new(&info_b, &b_)]).unwrap()[0].id();
    assert!(id_b > id_a);
}

#[test]
fn trained_dictionary_shrinks_blobs_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lodestar.db");
    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = (0..600)
        .map(|n| season(&format!("Show.Number.{n}.2024"), 3 + n % 10))
        .collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<_> = infos.iter().zip(&raws).map(|(i, r)| new(i, r)).collect();
    store.insert(&batch).unwrap();
    let blob_bytes = |s: &Store| -> i64 {
        s.connection()
            .query_row("SELECT sum(length(data)) FROM torrent_files", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let before = blob_bytes(&store);

    assert_eq!(store.active_file_dict(), None);
    let dict = store.train_file_dict(1000, 16 << 10).unwrap().unwrap();
    assert_eq!(store.active_file_dict(), Some(dict));
    assert_eq!(store.recompress_files(100).unwrap(), 600);
    assert_eq!(store.recompress_files(100).unwrap(), 0, "idempotent");
    let after = blob_bytes(&store);
    assert!(after * 2 < before, "{after} vs {before}");
    drop(store);

    let mut store = Store::open(&path, StoreConfig::default()).unwrap();
    assert_eq!(store.active_file_dict(), Some(dict));
    for (n, info) in infos.iter().enumerate() {
        let tree = store.files(n as i64 + 1).unwrap().unwrap();
        assert_eq!(tree.files, info.files);
    }
}

#[test]
fn too_few_samples_skip_training() {
    let mut store = mem();
    let raw = season("x", 2);
    let info = Info::parse(&raw).unwrap();
    store.insert(&[new(&info, &raw)]).unwrap();
    assert_eq!(store.train_file_dict(100, 16 << 10).unwrap(), None);
}
