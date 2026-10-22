# X-pack-static: Unraid templates, systemd unit, operations docs (draft)
**Model:** Sonnet (helper). **Branch:** `slice/X-pack-static`.
**Spec:**
- `docs/owner-decisions.md` and `runner/standards.md` (branch `runner/standards`, PR #39, until it merges);
- architecture §11 (Deployment: Docker, Unraid, Windows, systemd; ports 8090/tcp web and 6881/udp+tcp DHT/peers; `/data` volume; PUID/PGID/TZ; Unraid pool paths, not `/mnt/user`; FUSE warning; split hot/cold paths; gluetun variant; worker mode `LODESTAR_JOIN`/`LODESTAR_TOKEN`, cluster port 8091), §12 (security: setup token in logs, password) and §19.6 (joining workers);
- `crates/lodestar/src/config.rs` and `main.rs` for the **real** CLI and env variables today (`LODESTAR_DATA_DIR`, `LODESTAR_HTTP_BIND`, `LODESTAR_DHT_PORT`, `LODESTAR_DHT_ENABLED`, `LODESTAR_INTENSITY`, `LODESTAR_LOG`; `serve`, `healthcheck`, `import-bitmagnet`). Document what exists. Mark planned items (worker mode, the B2 settings UI, backup/rollback) clearly as "planned", never as working.

## Build
1. **`packaging/unraid/lodestar.xml`:** the Community Applications template.
   - WebUI `http://[IP]:[PORT:8090]`.
   - Ports 8090/tcp and 6881/udp + 6881/tcp.
   - `/data` → `/mnt/cache/appdata/lodestar` (pool path, with the description explaining why not `/mnt/user`).
   - PUID 99 / PGID 100, TZ.
   - Optional split paths `/data/hot` and `/data/cold`, with the caveats from §11.
   - Icon: the raw GitHub URL of `packaging/icons/lodestar-256.png`, which W3 is producing. Use that path.
   - Overview text in UX-writing style (§10.14), with no AI clichés.
   - Repository and support URLs: the GitHub repo.
2. **`packaging/unraid/lodestar-gluetun.xml`:** the VPN variant (`--network=container:gluetun`), with notes on port forwarding.
3. **`packaging/unraid/lodestar-worker.xml`:** the worker variant, marked "requires Lodestar v0.6+".
4. **`packaging/systemd/lodestar.service`:**
   - hardened: `DynamicUser` or a dedicated user, `StateDirectory`, `ProtectSystem=strict`, `NoNewPrivileges`, an `AF_INET`/`AF_INET6`/`AF_UNIX` allow list, `LimitNOFILE` sized for 400 sockets plus headroom;
   - `Nice=` for below-normal priority (§11 asks this of Windows; mirror the intent);
   - plus a README for installation.
5. **`docs/operations.md` (draft):**
   - install on each target;
   - first run (setup token in the logs);
   - data directory layout (§4);
   - networking: bridge with UDP mapping vs host, IPv6, VPN with gluetun, the home-router conntrack warning;
   - Unraid storage guidance;
   - upgrades and backups (planned items marked);
   - troubleshooting (FUSE path warning, port reachability).
6. **Validation:**
   - a small script or test that parses the three XML templates (well-formed, required CA fields present);
   - `systemd-analyze verify` on the unit if available in the container; otherwise note that it's unavailable.

## Don't
- Create the Dockerfile, release workflow or MSI. Those are D-docker and M6 later, after B3.
- Edit code.

## Done when
- The templates parse, and the CA required fields are present.
- The unit verifies (or the PR notes the tool is unavailable).
- `docs/operations.md` has no claim about a feature that doesn't exist yet unless it's marked planned.
