# Lodestar

*Every node is a new discovery.*

Lodestar is a from-scratch DHT crawler and torrent-metadata search engine. It ships as a single Rust binary with an embedded Svelte UI and runs on Docker, Unraid or Windows.

The plan lives in [`docs/architecture.md`](docs/architecture.md). The brand assets are in [`brand/source/`](brand/source/), checksummed in `SHA256SUMS.txt`.

The search syntax is documented in [`docs/query-language.md`](docs/query-language.md).

Status: milestone M0 (foundations) is in review: Cargo workspace, CI, `bencode`, `metainfo`, `store` and the synthetic-data size benchmark (`cargo run --release -p lodestar-bench --bin size-report`).

## Documentation

Setup and safety guide for Unraid and Docker: [`docs/user/`](docs/user/README.md). It covers installing, first run, keeping Lodestar private and backups. Windows and Linux chapters are planned.

## Licence

Lodestar is free software. It's licensed under the [GNU Affero General Public License, version 3 or later](LICENSE) (`AGPL-3.0-or-later`).

If you run a modified version of Lodestar for other people over a network, the AGPL asks you to offer them the source of your version. The app's About screen links to the source of the version it runs.

The name, logo, mascot and other brand artwork are **not** covered by the AGPL. See [`brand/LICENSE.md`](brand/LICENSE.md).
