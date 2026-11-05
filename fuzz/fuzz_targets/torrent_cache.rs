//! The torrent-cache plugin's reply handling (SRC-13) never panics. A
//! cache's body is untrusted: the plugin parses it as a `.torrent` under
//! its own caps, re-parses the info dict's raw bytes with no encoding
//! hint, and keeps it only when that dict hashes to the lookup's key.
//!
//! The plugin does this inside an async method that fetches the body, so
//! the target runs the same steps over the bytes, with the plugin's
//! `LIMITS` and its `verify`. The key is either the info dict's own SHA-1
//! (first byte even) or one taken from the input (odd), so both a match
//! and a mismatch are reached.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::{Info, InfoHash, ParseOptions, TorrentFile};
use lodestar_sources_plugins::torrent_cache::{LIMITS, STREAM, verify};
use sha1::{Digest, Sha1};
use sha2::Sha256;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else {
        return;
    };
    // The stream cap is the file cap: nothing larger reaches the parser.
    assert_eq!(LIMITS.max_torrent_bytes, STREAM.max_bytes as usize);
    let Ok(mut torrent) = TorrentFile::parse_with(body, &LIMITS) else {
        return;
    };
    let info_raw = TorrentFile::raw_info(body, &LIMITS).expect("a parsed file has an info dict");
    // The info dict is the file's own bytes, not a re-encoding.
    let start = info_raw.as_ptr() as usize - body.as_ptr() as usize;
    assert!(start + info_raw.len() <= body.len());
    let Ok(info) = Info::parse_with(
        info_raw,
        &ParseOptions {
            limits: LIMITS,
            encoding_hint: None,
        },
    ) else {
        return;
    };
    let sha1 = InfoHash(Sha1::digest(info_raw).into());
    let sha256: [u8; 32] = Sha256::digest(info_raw).into();
    if let Some(v1) = info.v1 {
        assert_eq!(v1, sha1, "v1 is the raw dict's SHA-1");
    }
    if let Some(v2) = info.v2 {
        assert_eq!(v2.0, sha256, "v2 is the raw dict's SHA-256");
    }
    torrent.info = info;
    let key = if pick % 2 == 0 {
        sha1
    } else {
        let mut k = [0u8; 20];
        let n = body.len().min(20);
        k[..n].copy_from_slice(&body[..n]);
        InfoHash(k)
    };
    match verify(&torrent, &key) {
        Some(false) => assert_eq!(torrent.info.v1, Some(key)),
        Some(true) => assert_eq!(torrent.info.v2.map(|h| h.truncated()), Some(key)),
        None => {
            assert_ne!(torrent.info.v1, Some(key));
            assert!(pick % 2 == 1 || torrent.info.v1.is_none());
        }
    }
});
