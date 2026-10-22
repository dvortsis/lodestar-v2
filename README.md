# Lodestar

*Every node is a new discovery.*

Lodestar is a from-scratch DHT crawler and torrent-metadata search engine. It ships as a single Rust binary with an embedded Svelte UI and runs on Docker, Unraid or Windows.

The plan lives in [`docs/architecture.md`](docs/architecture.md). The brand assets are in [`brand/source/`](brand/source/), checksummed in `SHA256SUMS.txt`.

The search syntax is documented in [`docs/query-language.md`](docs/query-language.md).

Status: milestone M0 (foundations) is in review: Cargo workspace, CI, `bencode`, `metainfo`, `store` and the synthetic-data size benchmark (`cargo run --release -p lodestar-bench --bin size-report`).
