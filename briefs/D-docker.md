# D-docker: Docker image, compose example, release workflow, smoke job
**Model:** Sonnet (helper). **Branch:** `slice/D-docker`.
**Spec:**
- `runner/standards.md` (the bar);
- architecture §11 (Deployment: allocator, Docker, Unraid, CLI `healthcheck`) and §12 (upgrades: Docker tags `x.y.z`, `x.y`, `latest`, `nightly`);
- `docs/build-plan.md` P1 "Docker" bullet and the v0.5 exit (`docker compose up` reaches `/healthz`);
- the code as it is: `crates/lodestar/src/main.rs` (CLI, `healthcheck`, env vars), `crates/lodestar/src/config.rs` (`LODESTAR_DATA_DIR`, `LODESTAR_HTTP_BIND`, `LODESTAR_DHT_PORT`, …), `crates/lodestar/build.rs` + `src/web.rs` (the SPA is embedded from `web/build` at compile time; a stub is served when it's absent), `packaging/unraid/*.xml` (what the templates expect of the image).

## Own
`packaging/docker/**`, `docker-compose.yml`, `.dockerignore` (new, repo root), `.github/workflows/release.yml` (new). Nothing else. Anything else that needs changing goes in the PR description.

## Build
1. **`packaging/docker/Dockerfile`** (build context = repo root):
   - Stage 1, Node (pin the image by version): `cd web && npm ci && npm run build`, so the binary embeds the real SPA.
   - Stage 2, Rust: a **static musl** build of `lodestar` for `linux/amd64` and `linux/arm64`, release profile. mimalloc is already the allocator (`crates/lodestar/Cargo.toml`); keep it. Cross-compile arm64 rather than emulating the whole Rust build under QEMU (`cargo-zigbuild` or a musl cross toolchain, using `TARGETPLATFORM`/`BUILDPLATFORM`). Use BuildKit cache mounts for the cargo registry and target dir.
   - Final: `gcr.io/distroless/static-debian12:nonroot`, pinned by digest. Copy only the binary.
   - `ENV LODESTAR_DATA_DIR=/data`, `LODESTAR_HTTP_BIND=0.0.0.0:8090` (check the exact variable and value format in `config.rs`), `VOLUME /data`, `EXPOSE 8090/tcp 6881/udp 6881/tcp`.
   - `HEALTHCHECK CMD ["/lodestar", "healthcheck"]` (exec form; **distroless has no shell**). Check that `healthcheck` reaches the bind address it'll be using.
   - `ENTRYPOINT ["/lodestar"]`, `CMD ["serve"]` (match the real CLI).
   - OCI labels: source, licence, version from build args.
   - Report the final image size in the PR (§11 targets about 30 MB).
2. **`docker-compose.yml`** (repo root): the image `ghcr.io/dvortsis/lodestar-v2:latest`, the `/data` volume, the 8090/tcp and 6881/udp+tcp ports, `TZ`, a `user:` line, a restart policy. Comments explain bridge networking with UDP mapping vs `network_mode: host` for better DHT reachability, and IPv6 (§11).
3. **`packaging/docker/README.md`**: build, run, ports, volume, tags, user and permissions, and the data layout **as the code writes it** (`lodestar.db`, `index/`, `dht-state.bin`, `auth.json`, `seal.key`, `config.toml`). §4's layout differs; follow the code and note the difference in the PR.
4. **`.github/workflows/release.yml`**:
   - Triggers: tags `v*.*.*` → `x.y.z`, `x.y`, `latest`; a daily schedule plus `workflow_dispatch` → `nightly`; `pull_request` touching `packaging/docker/**`, `docker-compose.yml`, `.dockerignore`, `crates/**`, `web/**`, `Cargo.*` or this workflow → **build and smoke only, never push**.
   - Buildx multi-arch (amd64 and arm64), pushed to `ghcr.io/dvortsis/lodestar-v2` with `GITHUB_TOKEN` (`packages: write` only on the jobs that push; everything else `contents: read`). Use the `gha` build cache. Pin every action by full commit SHA, with a version comment.
   - **Smoke job** (on every trigger, before any push): build amd64, `docker run` with a temporary `/data`, wait for the Docker health status to become `healthy` (with a timeout), `curl /healthz` and `/` (expect the SPA's `index.html`, not the stub), then `docker compose -f docker-compose.yml config` to validate the compose file. Fail on any error.
   - Concurrency group per ref.
5. **`.dockerignore`**: `target/`, `web/node_modules/`, `web/build/`, `.git/`, scratch and test output, so the context stays small.

## Known constraints (carried forward from earlier reviews)
- The image must set `LODESTAR_DATA_DIR=/data`; the Unraid templates rely on it.
- The templates reference `ghcr.io/dvortsis/lodestar-v2:latest`. Keep that name.
- Distroless has no shell, so nothing may rely on `sh -c` (healthcheck, entrypoint, templates).
- **PUID/PGID:** §11 says the image supports them, and the Unraid template sets them, but the binary doesn't handle them yet. Dropping privileges is security code, reserved for an Opus slice, so **don't implement it**. Run as the distroless `nonroot` user by default. Have the compose file and README use `user: "<uid>:<gid>"` to choose the account, and state the gap clearly in the PR description: the Unraid template needs `--user 99:100` in ExtraParams until the binary supports PUID/PGID.

## Don't
- Edit `crates/**`, `web/**`, `packaging/unraid/**`, `ci.yml` or `web.yml`.
- Add Rust or npm dependencies.
- Push images from pull-request runs.

## Done when
- `docker buildx build --platform linux/amd64 -f packaging/docker/Dockerfile .` succeeds locally, if Docker is available in your session. If it isn't, say so in the PR; the workflow's smoke job on the PR is then the proof, and it must be green.
- The PR's `release.yml` smoke job is green: healthy container, `/healthz` ok, `/` returns the real SPA, and compose config is valid.
- `actionlint` (if you can install it) and `hadolint` (same) are clean, or the PR explains why not.
- The PR description gives the image size and the arm64 build approach, and lists the PUID/PGID gap and the §4 data-layout difference.
