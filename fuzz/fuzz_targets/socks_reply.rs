//! A SOCKS5 proxy's replies (NOS-2a) never panic, and what they yield is
//! bounded: `reply_len` asks for `MIN_REPLY` to `MAX_REPLY` bytes, `parse_reply`
//! accepts only exactly that many, and a bound name is 1 to 255 bytes of
//! printable ASCII.
//!
//! Input: the first two bytes as the method choice and the login reply,
//! the rest as a `CONNECT` reply.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_integrations::net::socks::{
    Bound, MAX_REPLY, MIN_REPLY, REPLY_PREFIX, parse_auth, parse_method, parse_reply, reply_len,
};

fuzz_target!(|data: &[u8]| {
    let [a, b, reply @ ..] = data else {
        return;
    };
    let _ = parse_method([*a, *b], false);
    let _ = parse_method([*a, *b], true);
    let _ = parse_auth([*a, *b]);
    let want = reply
        .first_chunk::<REPLY_PREFIX>()
        .and_then(|p| reply_len(*p).ok());
    if let Some(n) = want {
        assert!((MIN_REPLY..=MAX_REPLY).contains(&n));
    }
    match parse_reply(reply) {
        Ok(bound) => {
            assert_eq!(want, Some(reply.len()));
            if let Bound::Name(name, _) = bound {
                assert!(!name.is_empty() && name.len() <= 255);
                assert!(name.bytes().all(|c| c.is_ascii_graphic()));
            }
        }
        Err(_) => {}
    }
});
