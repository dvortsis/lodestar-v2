# Lodestar image (architecture §11): a static musl `lodestar` on distroless.
# Build context is the repository root:
#
#   docker buildx build -f packaging/docker/Dockerfile -t lodestar .
#   docker buildx build --platform linux/amd64,linux/arm64 -f packaging/docker/Dockerfile .
#
# Both stages that compile run on the *build* platform (--platform=
# $BUILDPLATFORM). The arm64 binary is cross-compiled with cargo-zigbuild,
# so nothing runs under QEMU, and the final stage only copies a file.

# ---- 1. Web UI -------------------------------------------------------------
# Node 22 (web/package.json: engines >= 22). The binary embeds web/build at
# compile time (crates/lodestar/build.rs), so this stage runs first. The
# build's `assets` step reads ../brand/source and writes ../packaging/icons,
# so web/ sits at the same depth as in the repository.
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim@sha256:de170b39439eb0e5a3444ad7d60b015614a381e10fecae2e30ec9e735122f38d AS web
WORKDIR /src/web
COPY brand/source /src/brand/source
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci --no-audit --no-fund
COPY web/ ./
# The UI's version name is the package version plus this short SHA (web/scripts/build-version.mjs).
ARG REVISION=
RUN GITHUB_SHA="$REVISION" npm run build

# ---- 2. Rust binary --------------------------------------------------------
FROM --platform=$BUILDPLATFORM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS build
ARG TARGETARCH
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
# Pin the toolchain to the image's own; rust-toolchain.toml says "stable",
# which would otherwise make rustup download whatever is newest. The release
# profile keeps line tables (Cargo.toml); the image drops them (~100 MB).
# Built without the `fixture` feature (`serve --fixture` test data), like every
# release artifact.
ENV RUSTUP_TOOLCHAIN=1.97.1 \
    CARGO_TERM_COLOR=never \
    CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_DEBUG=false \
    CARGO_PROFILE_RELEASE_STRIP=symbols

# zig is the C compiler and linker for the musl targets (ring needs a C
# compiler). It comes from the PyPI `ziglang` wheel (a zip holding a static zig
# binary), checked against a pinned SHA-256. No apt or pip is needed.
ARG ZIG_WHEEL_AMD64=https://files.pythonhosted.org/packages/fe/6f/e1c380fe61db36cb464d5d4d18c557338812500986885373e2550d4ebf6b/ziglang-0.17.0-py3-none-manylinux_2_12_x86_64.manylinux2010_x86_64.musllinux_1_1_x86_64.whl
ARG ZIG_SHA256_AMD64=97079827064a7492cce5541ceb438ae8003ce1e54e399676da2d62fb7a5dc08b
ARG ZIG_WHEEL_ARM64=https://files.pythonhosted.org/packages/02/21/abee2f0524f54fe3e8783748b9df79bb07d4b955e6b465677d1cfbda335c/ziglang-0.17.0-py3-none-manylinux_2_17_aarch64.manylinux2014_aarch64.musllinux_1_1_aarch64.whl
ARG ZIG_SHA256_ARM64=d2804f94f54f7fcf8169f4b212a619ad688a4e2e6592e976f0cf2e58e2cd8af3
ARG BUILDARCH
RUN case "$BUILDARCH" in \
      amd64) url=$ZIG_WHEEL_AMD64; sha=$ZIG_SHA256_AMD64 ;; \
      arm64) url=$ZIG_WHEEL_ARM64; sha=$ZIG_SHA256_ARM64 ;; \
      *) echo "unsupported BUILDARCH: $BUILDARCH" >&2; exit 1 ;; \
    esac \
    && curl --fail --silent --show-error --location --retry 3 --output /tmp/ziglang.whl "$url" \
    && echo "$sha  /tmp/ziglang.whl" | sha256sum --check - \
    && unzip -q /tmp/ziglang.whl 'ziglang/*' -d /opt \
    && chmod +x /opt/ziglang/zig \
    && ln -s /opt/ziglang/zig /usr/local/bin/zig \
    && rm /tmp/ziglang.whl \
    && zig version

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    CARGO_TARGET_DIR=/tmp/zigbuild-target cargo install --locked cargo-zigbuild@0.24.0

RUN rustup target add --toolchain "$RUSTUP_TOOLCHAIN" x86_64-unknown-linux-musl aarch64-unknown-linux-musl

WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY bench ./bench
COPY --from=web /src/web/build ./web/build

# The target dir is a cache mount, so the binary is copied out in the same RUN.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target,id=lodestar-target-${TARGETARCH} \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-musl ;; \
      arm64) triple=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && cargo zigbuild --locked --release -p lodestar --bin lodestar --no-default-features --features web --target "$triple" \
    && install -D "target/$triple/release/lodestar" /out/lodestar \
    && mkdir -p /out/data \
    && install -d -m 0700 /out/witness

# ---- 3. Runtime ------------------------------------------------------------
# distroless/static-debian12:nonroot, no shell. It starts as root (USER below)
# only so the binary can apply PUID/PGID: before it reads the config, binds a
# socket or starts a thread, it hands /data to PUID:PGID (default 65532,
# "nonroot") and drops root for good (crates/lodestar/src/privdrop.rs).
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab

ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED=unknown
LABEL org.opencontainers.image.title="Lodestar" \
      org.opencontainers.image.description="A DHT crawler and torrent-metadata search engine" \
      org.opencontainers.image.source="https://github.com/dvortsis/lodestar-v2" \
      org.opencontainers.image.url="https://github.com/dvortsis/lodestar-v2" \
      org.opencontainers.image.licenses="NOASSERTION" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}"

COPY --from=build /out/lodestar /lodestar
# An empty /data owned by nonroot: a new named volume copies this ownership,
# so the default PUID/PGID start without a chown. (Bind mounts keep the host's
# owner until the binary changes it.)
COPY --from=build --chown=65532:65532 /out/data /data
# The restart proof's witness (dht.safe_restart, CR-0d): in the container
# layer, outside /data, so restoring a backup of /data can't roll it back.
# The binary hands it to PUID:PGID like /data. Recreating the container
# (every image update) loses it, so the first start after an update
# waits out the full sampling suppression.
COPY --from=build --chown=65532:65532 /out/witness /var/lib/lodestar-witness

# 0.0.0.0 so the published port reaches it (config.rs: a SocketAddr);
# `lodestar healthcheck` probes 127.0.0.1 at the same port.
ENV LODESTAR_DATA_DIR=/data \
    LODESTAR_DHT_WITNESS_DIR=/var/lib/lodestar-witness \
    LODESTAR_HTTP_BIND=0.0.0.0:8090

# Root, so PUID/PGID can be applied; the binary drops it at once. `--user`
# (or compose `user:`) starts it unprivileged instead, which skips the drop.
USER 0:0

VOLUME /data
EXPOSE 8090/tcp 6881/udp 6881/tcp

# Exec form: there is no shell in this image. It runs as the image's user
# (root) and drops to PUID/PGID like the server; it needs no privilege.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["/lodestar", "healthcheck"]

ENTRYPOINT ["/lodestar"]
CMD ["serve"]
