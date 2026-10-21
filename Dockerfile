# Static musl build on distroless/static (architecture §11).
# Multi-arch: docker buildx build --platform linux/amd64,linux/arm64 .
# Each platform builds natively on Alpine (musl), so the binary is static
# without a cross toolchain. mimalloc, zstd and SQLite need a C compiler.
FROM rust:1-alpine AS build
RUN apk add --no-cache build-base
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY bench bench
RUN cargo build --release --locked -p lodestar \
    && strip target/release/lodestar

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /src/target/release/lodestar /usr/local/bin/lodestar
ENV LODESTAR_DATA_DIR=/data
VOLUME /data
EXPOSE 8090/tcp 6881/udp 6881/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/usr/local/bin/lodestar", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/lodestar"]
CMD ["serve"]
