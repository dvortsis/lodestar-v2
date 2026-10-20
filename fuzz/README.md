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

CI runs each target for a short time on every pull request (`fuzz` job). Crashes land in `fuzz/artifacts/<target>/`. Minimise one with `cargo +nightly fuzz tmin <target> <file>`, then add it as a regression test in the owning crate.
