# lodestar-companions: the companion manager core

The start of Lodestar's companion manager (architecture §13, §18.10; plan `docs/plan/2026-11-06-v06-companions.md`). This crate holds the companion catalogue, the adapter traits and the closed input types. Fetching, extraction, credentials, supervision and the API arrive in later CMP slices, each as its own module.

**This file is the contract** that CMP-13's pin-refresh tool writes and every later CMP slice reads.

## Trust root
- **The catalogue is compiled in** (owner §2.46, cq1 (a)). `data/catalogue.json` is embedded with `include_str!`, so Lodestar's release signature (§12.2) covers it, and it changes only with a release.
  - **First installs:** these are unsigned downloads (owner §2.23), so the catalogue is only as trustworthy as wherever Lodestar was downloaded from.
- **The SHA-256 pin is the run-time gate** (owner §2.46, cq2). Upstream OpenPGP signatures are checked once, when CMP-13 makes the pin, and recorded as `verified_at_pin`. The binary contains no OpenPGP code.
- **A release reads no other catalogue.**
  - `catalogue::builtin()` is the only way in.
  - `Catalogue::from_json` exists only under `cfg(test)` or the `testing` feature, and no shipped build enables that feature.
  - Nothing reads a catalogue from the configuration or the environment.
- **Invalid means off, not a crash.** If the compiled-in catalogue doesn't validate:
  - `builtin()` returns the error and logs `companions.catalogue_invalid` with the rule's code;
  - companion installs are off;
  - Lodestar keeps running.
  - This crate's tests fail on such a catalogue first, so it never ships.

## Format (`format: 1`)
Parsing is strict JSON:
- unknown fields, duplicate keys and other types are refused;
- integers must be whole and in range;
- every vocabulary is closed.

The document is at most 256 KiB and has 1–64 entries.

| Field | Rule |
|---|---|
| `id` | `[a-z][a-z0-9-]{0,31}`, not ending in `-`; unique |
| `name` | 1–64 bytes: ASCII letters, digits, space, `.+()-`; no leading or trailing space |
| `licence` | `{spdx, url}`. `spdx` is SPDX ids joined by ` AND `, ` OR ` or ` WITH `. `url` is an `https` URL |
| `homepage` | an `https` URL |
| `needed_for` | 1–4 unique values from `download_client`, `indexer_manager`, `vpn_route`, `challenge_solver` |
| `opt_in` | `true` for anything off and not installed by default. **Required for `challenge_solver`** (owner §2.47, Q21) |
| `hosts` | 0–8 unique hosts for downloads and their redirects. Each must be in `DOWNLOAD_HOSTS` (`github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`). Not empty if `native` is set; empty if it isn't |
| `native` | `null`, or `{artifacts: [...]}` with 1–8 artifacts, one per platform |
| `container` | `null`, or `{image, digest, name}` |
| `ports` | 1–16 ports, each `{name, port, proto, exposure}`. Names are unique, and so are number and protocol pairs |
| `health` | `null`, or `{port, path}`. `port` names an `internal` TCP port. `path` is `/` followed by `[A-Za-z0-9/._-]`, with no `.` or `..` segment |
| `templates` | 0–8 unique values from `qbittorrent_conf`, `prowlarr_config_xml` (both need `native`), `compose_service` and `unraid_template` (both need `container`) |

Each entry has at least one of `native` and `container`. An entry needed for `vpn_route` is container-only, because the native VPN is v1.x.

**Artifacts:**

| Field | Rule |
|---|---|
| `platform`, `arch` | One of the pairs v0.6 supports: `linux` with `x86_64` or `aarch64`, or `windows` with `x86_64` |
| `archive` | `zip`, `tar_gz`, `binary` (the file is the executable) or `installer` (Windows only; offered for you to run, plan Q4) |
| `executable` | Required for `zip` and `tar_gz`, `null` otherwise. A path inside the archive: `/`-separated `[A-Za-z0-9._-]` components, with no `.` or `..` component and nothing absolute; at most 16 components and 255 bytes. It ends in `.exe` exactly when the platform is Windows |
| `pin` | `null` until CMP-13 pins it, or `{version, url, size, sha256, upstream_sig}` |

**Pins:**

| Field | Rule |
|---|---|
| `version` | 1–4 dot-separated decimal numbers, each with no leading zero and at most 9 digits. All pins in an entry share one version |
| `url` | `https`, with no port, user information, query, fragment or percent-encoding. The path is `[A-Za-z0-9-._~/+]` with no empty, `.` or `..` segment. The host must be one of the entry's `hosts`. The file name must match the archive kind: `.zip`; `.tar.gz` or `.tgz`; `.exe` or `.msi` for an installer; for a bare binary, `.exe` exactly on Windows |
| `size` | Exact, `1..=536870912` (512 MiB, plan Q16) |
| `sha256` | 64 lower-case hex digits, and not one digit repeated (a placeholder) |
| `upstream_sig` | `{"kind": "none"}`, or `{"kind": "verified_at_pin", "fingerprint": "<40 or 64 upper-case hex>"}` |

**Containers:**

| Field | Rule |
|---|---|
| `image` | `<registry>/<1–3 components>`. The registry is in `REGISTRIES` (`lscr.io`, `ghcr.io`, `docker.io`). Components are `[a-z0-9]+([._-][a-z0-9]+)*`. **No tag:** images are pinned by digest |
| `digest` | `null` until pinned, or `sha256:<64 lower-case hex>` |
| `name` | The container and compose service name that detection looks for. Lower-case Docker name rules; unique |

**Ports:** numbers are `1024..=65535`, `proto` is `tcp` or `udp`, and `exposure` is either:
- `internal`: bound to `127.0.0.1`, or to the internal network in a container;
- `p2p`: published. Only an entry needed for `download_client` may have one.

**Errors:**
- Each error names the rule (`Rule::code()`, a stable string) and where it happened, such as `entries[0].native.artifacts[1].pin.url`, or a line and column for a syntax error.
- The location is built from the code's own field names and indexes, never from catalogue text.

## Unpinned entries
The v0.6 entries follow owner §2.47 (cq6):
- **qBittorrent:** the userdocs static `qbittorrent-nox` on Linux x86_64 and aarch64 (Windows is CMP-11's), and `lscr.io/linuxserver/qbittorrent`;
- **Prowlarr:** upstream builds for Linux x86_64 and aarch64 and Windows x64, and `lscr.io/linuxserver/prowlarr`;
- **gluetun:** container only, `docker.io/qmcgaw/gluetun`;
- **FlareSolverr:** comes with CMP-12, opt-in.

**Every artifact and image ships unpinned until CMP-13's tool pins it.**
- `Entry::installable` returns `None` for an unpinned artifact, and `Container::pinned_digest` returns `None` for an unpinned image.
- CMP-1, CMP-5 and CMP-7 install and render only what those return.

## Adapter traits (`adapter`)
- **`Detect`, `Health` and `Configure`** are object-safe and async, returning boxed futures. They take a `Target`, which is one of:
  - loopback on a port;
  - a catalogue container name and a port;
  - a URL you entered (`EnteredUrl`), which is checked for shape only.
- **Implementations must:**
  - send every request through the SSRF guard's companion mode;
  - apply a timeout and a byte cap;
  - never follow a redirect off the target.
- **Results are closed values:**
  - a strict `Version`;
  - a `HealthStatus`;
  - an `AdapterError` that carries only the code's own words.
- **`Configure::plan` lists setting names** (`SettingKey`, never values). The plan is shown before `apply` runs, and `apply` changes nothing the plan doesn't list.

## Input types (`inputs`)
Generated compose services and Unraid templates are built only from these validated values:

| Type | Accepts | Refuses |
|---|---|---|
| `HostPath` | Absolute Linux paths with `[A-Za-z0-9._ -]` components; at most 255 bytes | `/`; `/etc`, `/proc`, `/run`, `/opt`, `/var` and the other system folders (under `/var`, only `/var/lib/lodestar` and `/var/spool/lodestar` are allowed); any `*.sock` |
| `Puid`, `Pgid` | `1..=2147483647` | `0` (root) |
| `PortNumber` | `1024..=65535` | Anything lower |
| `ContainerName` | Lower-case Docker names | Anything else |
| `NetworkName` | Lower-case Docker names | `host`, `none`, `default`, `bridge` |
| `NetworkMode` | A named network, or `service:gluetun` | Anything else |

None of them can hold a character that means something to YAML, compose interpolation (`$`), volume syntax (`:`), XML or a shell. CMP-7 still quotes every value it writes.
