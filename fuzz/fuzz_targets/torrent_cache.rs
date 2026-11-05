//! The torrent-cache plugin's reply handling (SRC-13) never panics. A
//! cache's body is untrusted: the plugin parses it as a `.torrent` under
//! its own caps, re-parses the info dict's raw bytes with no encoding
//! hint, and keeps it only when that dict hashes to the lookup's key.
//!
//! The plugin does this inside an async method that fetches the body, so
//! the target runs the same steps over the bytes, with the plugin's
//! `LIMITS` and its `verify`. The first byte picks the key, modulo 3: the
//! dict's v1 hash, its truncated v2 hash (v1 when it has none), or 20
//! bytes from the input, so the v1 match, the v2 match and the mismatch
//! are all reached.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::{Info, InfoHash, ParseOptions, TorrentFile};
use lodestar_sources_plugins::torrent_cache::{LIMITS, STREAM, verify};

// The stream cap is the file cap: nothing larger reaches the parser.
const _: () = assert!(LIMITS.max_torrent_bytes == STREAM.max_bytes as usize);

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else {
        return;
    };
    let Ok(mut torrent) = TorrentFile::parse_with(body, &LIMITS) else {
        return;
    };
    let info_raw = TorrentFile::raw_info(body, &LIMITS).expect("a parsed file has an info dict");
    let Ok(info) = Info::parse_with(
        info_raw,
        &ParseOptions {
            limits: LIMITS,
            encoding_hint: None,
        },
    ) else {
        return;
    };
    // `raw_info` hands back the span the file parse hashed: the re-parse
    // without the file's encoding hint names the same swarm.
    assert_eq!(
        torrent.info.v1, info.v1,
        "raw_info and parse_with disagree on v1"
    );
    assert_eq!(
        torrent.info.v2, info.v2,
        "raw_info and parse_with disagree on v2"
    );
    torrent.info = info;
    let v1 = torrent.info.v1;
    let v2 = torrent.info.v2.map(|h| h.truncated());
    let key = match pick % 3 {
        0 => v1.or(v2),
        1 => v2.or(v1),
        _ => {
            let mut k = [0u8; 20];
            let n = body.len().min(20);
            k[..n].copy_from_slice(&body[..n]);
            Some(InfoHash(k))
        }
    }
    .expect("a parsed info dict has a v1 or v2 hash");
    match verify(&torrent, &key) {
        Some(false) => assert_eq!(v1, Some(key)),
        Some(true) => {
            assert_eq!(v2, Some(key));
            assert_ne!(v1, Some(key), "a v1 match is reported as v1");
        }
        None => {
            assert!(pick % 3 == 2, "the dict's own hash always verifies");
            assert!(v1 != Some(key) && v2 != Some(key));
        }
    }
});
