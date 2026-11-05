//! The torrent-cache plugin's reply handling (SRC-13) never panics. A
//! cache's body is untrusted: the plugin checks it with `verify_body`,
//! which parses it as a `.torrent` under its own caps, re-parses the info
//! dict's raw bytes with no encoding hint, and keeps it only when that
//! dict hashes to the lookup's key. The target calls `verify_body`
//! itself (X-fuzz-cache-1105), the same function the plugin's async
//! lookup calls on each body it fetches.
//!
//! The first byte picks the key, modulo 3: the dict's v1 hash, its
//! truncated v2 hash (v1 when it has none), or 20 bytes from the input,
//! so the v1 match, the v2 match and the mismatch are all reached. The
//! target works the key out with its own parse, which also checks that
//! `raw_info`'s span names the same swarm as the file parse.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::{Info, InfoHash, ParseOptions, TorrentFile};
use lodestar_sources_plugins::torrent_cache::{
    LIMITS, Lookup, Miss, STREAM, Why, verify, verify_body,
};

// The stream cap is the file cap: nothing larger reaches the parser.
const _: () = assert!(LIMITS.max_torrent_bytes == STREAM.max_bytes as usize);

/// The dict's v1 and truncated v2 hashes, by the target's own parse, or
/// `None` when the body isn't a `.torrent` within `LIMITS`.
fn hashes(body: &[u8]) -> Option<(Option<InfoHash>, Option<InfoHash>)> {
    let torrent = TorrentFile::parse_with(body, &LIMITS).ok()?;
    let info_raw = TorrentFile::raw_info(body, &LIMITS).expect("a parsed file has an info dict");
    let info = Info::parse_with(
        info_raw,
        &ParseOptions {
            limits: LIMITS,
            encoding_hint: None,
        },
    )
    .ok()?;
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
    Some((info.v1, info.v2.map(|h| h.truncated())))
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else {
        return;
    };
    let parsed = hashes(body);
    let key = match (pick % 3, parsed) {
        (0, Some((v1, v2))) => v1.or(v2),
        (1, Some((v1, v2))) => v2.or(v1),
        _ => {
            let mut k = [0u8; 20];
            let n = body.len().min(20);
            k[..n].copy_from_slice(&body[..n]);
            Some(InfoHash(k))
        }
    }
    .expect("a parsed info dict has a v1 or v2 hash");
    let why = if pick & 0x80 == 0 {
        Why::Unverified
    } else {
        Why::Interest
    };
    let lookup = Lookup::new(key, why, false).expect("a public key has a lookup");
    let got = verify_body(body.to_vec(), &lookup, "cache.example");
    let Some((v1, v2)) = parsed else {
        assert_eq!(
            got.err(),
            Some(Miss::Invalid),
            "an unparsable body is Invalid"
        );
        return;
    };
    match got {
        Ok(v) => {
            assert_eq!(v.key(), key);
            assert_eq!(v.why(), why);
            assert_eq!(v.host(), "cache.example");
            assert_eq!(verify(v.torrent(), &key), Some(v.is_v2()));
            if v.is_v2() {
                assert_eq!(v2, Some(key));
                assert_ne!(v1, Some(key), "a v1 match is reported as v1");
            } else {
                assert_eq!(v1, Some(key));
            }
            // The kept dict is the one the key names, parsed with no hint.
            let info = Info::parse_with(
                v.info_raw(),
                &ParseOptions {
                    limits: LIMITS,
                    encoding_hint: None,
                },
            )
            .expect("the kept dict parses");
            assert_eq!(&info, &v.torrent().info);
        }
        Err(m) => {
            assert_eq!(m, Miss::Mismatch, "a parsed body is only ever a mismatch");
            assert!(pick % 3 == 2, "the dict's own hash always verifies");
            assert!(v1 != Some(key) && v2 != Some(key));
        }
    }
});
