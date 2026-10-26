# Linux tarball

`lodestar-<version>-<arch>-linux-musl.tar.gz` for `x86_64` and `aarch64`. Each
holds one static musl `lodestar` (built the same way as the Docker image, with the
web UI embedded), `LICENSE`, `README.txt` and `lodestar.service`. A
`.tar.gz.sha256` file sits beside each tarball.

Install steps are in `README.txt` inside the tarball. The systemd unit is
`packaging/systemd/lodestar.service` (see `packaging/systemd/README.md`); the
tarball script copies it rather than keeping a second version.

## Build and test

Install Node.js 22 or newer and the Rust musl target/toolchain required below.
From the repository root, build the frontend before the binary:

```sh
cd web
npm ci
npm run build
cd ..
cargo build --release --locked -p lodestar --no-default-features --features web --target x86_64-unknown-linux-musl
packaging/linux/build-tarball.sh x86_64-unknown-linux-musl 0.5.0 \
  target/x86_64-unknown-linux-musl/release/lodestar dist
packaging/linux/smoke.sh dist/lodestar-0.5.0-x86_64-linux-musl.tar.gz
```

Without `web/build`, Rust embeds a stub page rather than the application UI.
The package smoke check requires the full frontend; rebuild both frontend and binary if it is missing.

The smoke script checks the checksum and contents, starts `lodestar serve` in a
temporary data directory on `127.0.0.1:18090`, checks `/healthz` and the
embedded UI, then stops it with SIGTERM and expects exit code 0.

CI (`.github/workflows/release.yml`) builds the musl binaries with
`cargo-zigbuild`, like `packaging/docker/Dockerfile`. The `aarch64` tarball is
built but not run, because the runner is `x86_64`.

## Not covered

macOS tarballs (architecture §11 mentions them) are out of scope here.
