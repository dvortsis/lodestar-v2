//! zstd at the store's settings gives the same bytes for the same tree
//! and dictionary, whether the compressor is fresh or has compressed
//! other trees first (the store reuses one per connection).

use lodestar_bench::dataset::{self, Key};
use lodestar_store::rusqlite::{Connection, OpenFlags};
use zstd::bulk::{Compressor, Decompressor};
use zstd::zstd_safe::CParameter;

#[test]
fn a_reused_compressor_gives_a_fresh_ones_bytes() {
    let d = tempfile::tempdir().unwrap();
    let ds =
        dataset::build_until(d.path(), Key::new(7, 1200), &mut |_| {}, Some("stored")).unwrap();
    let c = Connection::open_with_flags(ds.db(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let dict: Vec<u8> = c
        .query_row("SELECT data FROM zstd_dicts", [], |r| r.get(0))
        .unwrap();
    let blobs: Vec<Vec<u8>> = c
        .prepare("SELECT data FROM torrent_files WHERE codec = 1 ORDER BY torrent_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut dec = Decompressor::with_dictionary(&dict).unwrap();
    let raws: Vec<Vec<u8>> = blobs
        .iter()
        .map(|b| dec.decompress(b, 1 << 30).unwrap())
        .collect();
    let make = || {
        let mut c = Compressor::with_dictionary(19, &dict).unwrap();
        c.set_parameter(CParameter::DictIdFlag(false)).unwrap();
        c.set_parameter(CParameter::ChecksumFlag(false)).unwrap();
        c
    };
    let mut reused = make();
    let again: Vec<Vec<u8>> = raws.iter().map(|r| reused.compress(r).unwrap()).collect();
    let stored = again.iter().zip(&blobs).filter(|(x, y)| x != y).count();
    let fresh = raws
        .iter()
        .zip(&again)
        .filter(|(r, x)| make().compress(r).unwrap() != **x)
        .count();
    assert_eq!(
        (stored, fresh),
        (0, 0),
        "of {} trees: differ from the stored blob, differ from a fresh compressor",
        raws.len()
    );
}
