//! Zero-copy bencode (BEP 3) decoder and canonical encoder.
//!
//! Everything this crate decodes comes from untrusted peers, so the decoder
//! is iterative (no recursion, so hostile nesting can't overflow the stack),
//! bounds every allocation by [`Limits`], and never panics on any input.
//!
//! Two decoding modes exist:
//!
//! * **lenient** ([`decode`]): accepts the non-canonical but unambiguous forms
//!   real clients emit (unsorted dictionary keys, leading zeros, `i-0e`).
//!   Duplicate keys and trailing bytes are still rejected, since they make a
//!   message ambiguous.
//! * **strict** ([`decode_strict`]): accepts only canonical bencode. A strict
//!   decode that succeeds re-encodes to exactly the input bytes.
//!
//! Decoded containers remember the raw input span they came from
//! ([`Dict::raw`], [`List::raw`]); that's how an info hash is computed over the
//! exact bytes a peer sent rather than over a re-encoding.
//!
//! The encoder ([`Encoder`], [`to_vec`]) always writes canonical bencode.

mod decode;
mod encode;
mod value;

pub use decode::{
    DecodeError, DecodeOptions, ErrorKind, Limits, decode, decode_prefix, decode_strict,
    decode_with,
};
pub use encode::{DictEncoder, Encode, Encoder, ListEncoder, to_vec};
pub use value::{Dict, List, OwnedValue, Value};
