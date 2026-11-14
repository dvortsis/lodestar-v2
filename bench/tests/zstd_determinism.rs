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

/// The tree a macOS build compressed differently (torrent 56), compressed
/// again many times: fresh compressors and the store's reused one, on
/// several threads at once. Every output must be the same.
#[test]
fn one_tree_compresses_to_one_output_however_often() {
    let d = tempfile::tempdir().unwrap();
    let ds =
        dataset::build_until(d.path(), Key::new(7, 1200), &mut |_| {}, Some("stored")).unwrap();
    let c = Connection::open_with_flags(ds.db(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let dict: Vec<u8> = c
        .query_row("SELECT data FROM zstd_dicts", [], |r| r.get(0))
        .unwrap();
    // As the store's recompression saw them: every blob up to torrent 56,
    // a raw one as it is.
    let blobs: Vec<(i64, Vec<u8>)> = c
        .prepare("SELECT codec, data FROM torrent_files WHERE torrent_id <= 56 ORDER BY torrent_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut dec = Decompressor::with_dictionary(&dict).unwrap();
    let raws: Vec<Vec<u8>> = blobs
        .iter()
        .map(|(codec, b)| {
            if *codec == 1 {
                dec.decompress(b, 1 << 30).unwrap()
            } else {
                b.clone()
            }
        })
        .collect();
    let stored = blobs.last().unwrap().1.clone();
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let (dict, raws) = (dict.clone(), raws.clone());
            std::thread::spawn(move || {
                let make = || {
                    let mut c = Compressor::with_dictionary(19, &dict).unwrap();
                    c.set_parameter(CParameter::DictIdFlag(false)).unwrap();
                    c.set_parameter(CParameter::ChecksumFlag(false)).unwrap();
                    c
                };
                let target = raws.last().unwrap();
                let mut outs = std::collections::BTreeMap::<Vec<u8>, usize>::new();
                for round in 0..40 {
                    let out = if (round + t) % 2 == 0 {
                        make().compress(target).unwrap()
                    } else {
                        let mut z = make();
                        for r in &raws[..raws.len() - 1] {
                            z.compress(r).unwrap();
                        }
                        z.compress(target).unwrap()
                    };
                    *outs.entry(out).or_default() += 1;
                }
                outs
            })
        })
        .collect();
    let mut all = std::collections::BTreeMap::<Vec<u8>, usize>::new();
    for t in threads {
        for (k, v) in t.join().unwrap() {
            *all.entry(k).or_default() += v;
        }
    }
    let sizes: Vec<(usize, usize, bool)> = all
        .iter()
        .map(|(k, v)| (k.len(), *v, *k == stored))
        .collect();
    assert_eq!(
        all.len(),
        1,
        "outputs (size, count, is the stored one): {sizes:?}"
    );
}
