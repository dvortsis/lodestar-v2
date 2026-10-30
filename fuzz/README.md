# Fuzz targets

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for every parser of untrusted input (architecture §6, §14 M1). They need a nightly toolchain:

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run krpc            # runs until stopped or a crash
cargo +nightly fuzz run metainfo -- -max_total_time=300
```

| Target | Checks |
|---|---|
| `bencode` | No panics. A strict decode that succeeds means the input was canonical. A lenient decode re-encodes to a stable canonical form. |
| `krpc` | No panics. Parsed messages are stable under encode → parse. |
| `metainfo` | No panics in `Info` or `TorrentFile` parsing. Parsed file lists keep the model's invariants and round-trip through the store's blob codec. |
| `filetree` | No panics on corrupt blobs. Accepted blobs round-trip. |
| `tracker_scrape` | No panics in the UDP (BEP 15) and HTTP (BEP 48) scrape reply parsers. Parsed replies never hold more entries than the request allows. |
| `release` | No panics in the release-name parser. Parses are deterministic, and reported groups and versions are text from the name. |
| `lists` | No panics in blocklist decoding, format detection and the hash and IP parsers; IP sets round-trip through their stored encoding. The content filter's normalizer and core never panic on the same text as a name and file list, and the verdict agrees with the full hit list. |
| `utp` | No panics in the uTP packet parser, and parsed packets round-trip. A connection fed hostile packets and local events keeps its buffers bounded and only sends packets that parse. |
| `dht_restart` | No panics in CR-0d's snapshot, marker and witness decoders. The target seals its input itself (the first byte picks the kind, and for a snapshot the next 8 bytes give the wall clock, to the nanosecond), so the body decoders are reached despite the checksum. Whatever decodes is canonical, and a decoded snapshot that resumes never seeds a deadline earlier than the one it records. |
| `update` | No panics in the update envelope, manifest, key statement, freshness, minisign and trust-state parsers, or in `verify::check` against a fixed test trust (also on bodies the fuzzer's bytes become once signed with a test key). A document that verifies re-verifies identically, as a refetch on the state it produced, and that state reloads. |

`seeds/<target>/` holds hand-made inputs a target should start from, such as the BEP 47 padding, symlink and mixed `path.utf-8` shapes for `metainfo` and their blobs for `filetree`. Pass the folder after the corpus so libFuzzer reads it but writes new finds to the (ignored) corpus:

```sh
cargo +nightly fuzz run metainfo fuzz/corpus/metainfo fuzz/seeds/metainfo -- -max_total_time=600
```

CI runs each target for a short time on every pull request (`fuzz` job). Crashes land in `fuzz/artifacts/<target>/`. Minimise one with `cargo +nightly fuzz tmin <target> <file>`, then add it as a regression test in the owning crate.
