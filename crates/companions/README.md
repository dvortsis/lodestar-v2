# lodestar-companions: the companion manager core

The start of Lodestar's companion manager (architecture §13, §18.10; plan `docs/plan/2026-11-06-v06-companions.md`). This crate holds the compiled-in companion catalogue, the adapter traits, the closed input types and the shared identifiers. Fetching, extraction, credentials, supervision and the API arrive in later CMP slices, each as its own module.

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
| `opt_in` | `true` for anything off and not installed by default. **Required for `challenge_solver`** (owner §2.47, Q21). An entry whose id, container name or image names FlareSolverr must be both `challenge_solver` and opt-in, however else it's labelled |
| `hosts` | 0–8 unique hosts for downloads and their redirects. Each must be in `DOWNLOAD_HOSTS` (`github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`). Not empty if `native` is set; empty if it isn't |
| `native` | `null`, or `{release, artifacts}`. `release` is the entry's release folder (`https://github.com/<owner>/<project>/releases/download/`): an `https` URL on one of the entry's hosts, at least two segments deep, ending in `/`. `artifacts` holds 1–8 artifacts, one per platform |
| `container` | `null`, or `{image, digest, name}` |
| `ports` | 1–16 ports, each `{name, port, proto, exposure}`. Names are unique, and so are number and protocol pairs |
| `health` | `null`, or `{port, path}`. `port` names an `internal` TCP port. `path` is an `adapter::RequestPath`, the same grammar every adapter request uses |
| `templates` | 0–8 unique values. `qbittorrent_conf` needs `native` and `download_client`. `prowlarr_config_xml` needs `native` and `indexer_manager`. `compose_service` and `unraid_template` need `container` |

Each entry has at least one of `native` and `container`. An entry needed for `vpn_route` is container-only, because the native VPN is v1.x.

**Artifacts:**

| Field | Rule |
|---|---|
| `platform`, `arch` | One of the pairs v0.6 supports: `linux` with `x86_64` or `aarch64`, or `windows` with `x86_64` |
| `archive` | `zip`, `tar_gz`, `binary` (the file is the executable) or `installer` (Windows only; offered for you to run, plan Q4) |
| `executable` | Required for `zip` and `tar_gz`, `null` otherwise. A path inside the archive: `/`-separated `[A-Za-z0-9._-]` components; nothing absolute; no component made only of dots or ending in `.` (Windows trims it), and no Windows device name (`CON`, `NUL`, `COM1`, …); at most 16 components and 255 bytes. It ends in `.exe` exactly when the platform is Windows |
| `pin` | `null` until CMP-13 pins it, or `{version, url, size, sha256, upstream_sig}` |

**Pins:**

| Field | Rule |
|---|---|
| `version` | 1–4 dot-separated decimal numbers, each with no leading zero and at most 9 digits. All pins in an entry name the same version, written identically |
| `url` | `https`, with no port, user information, query, fragment or percent-encoding. The path is `[A-Za-z0-9-._~/+]` with no empty, `.` or `..` segment. The host must be one of the entry's `hosts`, and the URL must be a file inside the entry's `release` folder (owner §2.47 cq6: no other owner or project on the same host). The file name must match the archive kind: `.zip`; `.tar.gz` or `.tgz`; `.exe` or `.msi` for an installer; for a bare binary, no extension at all on Linux and exactly `.exe` on Windows |
| `size` | Exact, `1..=536870912`: 512 MiB, plan Q16's "512 MB" read as binary megabytes |
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

**Every artifact and image ships awaiting its pin until CMP-13's tool pins it.**
- **Its own state:** "awaiting pin" is `Pinned::AwaitingPin` (`"pin": null` or `"digest": null` in the JSON), never an empty or placeholder hash. `Artifact::pin()` and `Container::digest()` hand out a hash only when one is pinned.
- **The APIs CMP-1, CMP-5 and CMP-7 call refuse it with a clear error.**
  - `Entry::installable(platform, arch)` returns the artifact and its pin, or a `NotAvailable` error:
    - `NoBuild`: no native builds;
    - `NoPlatform`: none for this platform;
    - `AwaitingPin`: in the catalogue but not pinned yet.
  - `Entry::renderable()` does the same for the container image. The image and digest are private and come only as a `PinnedImage`, which writes `registry/repository@sha256:<hex>`, so no output can fall back to an implicit `:latest`. A container's `name()` stays readable for detection.
  - `Entry::awaiting_pin()` says whether anything still waits.
- **A release must not offer to install anything still awaiting its pin.** That gate is CMP-13's release-workflow step, not this crate's.

## Adapter traits (`adapter`)
- **`Detect`, `Health` and `Configure`** are object-safe and async, returning boxed futures.
- **They take a `Target`**, which is one of:
  - loopback on a port;
  - a catalogue container name and a port;
  - a URL you entered (`EnteredUrl`).
- **An adapter reaches a companion only through the `&dyn GuardedHttp` it's handed.** It never holds a client of its own.
  - **Implementations of `GuardedHttp`** (CMP-8a, wired in by `crates/lodestar`) must:
    - connect only to the request's target, through the SSRF guard's companion mode;
    - follow no redirect off the target;
    - stop at the byte cap (`TooLarge`) and at the timeout (`Timeout`);
    - keep no state between requests.
  - **Every `ProbeRequest` carries `Limits`:** a timeout of 100 ms to 30 s and a byte cap of 1 B to 1 MiB. `Limits::PROBE` is 5 s and 64 KiB.
  - **A `ProbeResponse` can't be built over its cap,** and its fields are private.
  - **Paths are `RequestPath`s:** RFC 3986 characters and no fragment. Also refused:
    - `//`, and `.` or `..` segments, including the `%2e` forms;
    - `%2f`, `%5c`, `%25` or `;` before the query, so a server that decodes twice can't be steered;
    - any encoded byte from `0x80` before the query, which covers overlong and fullwidth spellings of `.` and `/` (`%c0%ae`, `%ef%bc%8e`);
    - `%00` anywhere.
  - **Headers:**
    - **Names** are `HeaderName` constants. Framing, target and hop-by-hop names are refused in any case: `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Keep-Alive`, `Proxy` and every `Proxy-*`, `TE`, `Trailer`, `Upgrade`, `Expect` and `HTTP2-Settings`.
    - **Values** are built through `HeaderValue::plain` or `HeaderValue::secret`. Both refuse CR, LF, NUL and other controls, so a credential can't inject a header either. A credential stays a `Secret` until the client writes it, and `Debug` never shows a value.
- **`EnteredUrl` checks shape only:**
  - `http` or `https`;
  - RFC 3986 characters only, refusing `\` and `"<>{}|^` and `` ` ``;
  - no query and no fragment: it's a base that request paths are joined to, and `http://h/#` would swallow them;
  - a base path that passes the same checks as a request path (no `..`, `%00`, `//` or `;`);
  - well-formed `%` escapes;
  - no user information;
  - a non-empty host of `[A-Za-z0-9.-]`, or bracketed IPv6;
  - a port of `1..=65535`.

  Where it points is the guard's call at connect time. **It's never rendered into a template.**
- **Results are closed values:**
  - a strict `Version`;
  - a `HealthStatus`;
  - an `AdapterError` that carries only the code's own words.
- **`Configure` plans first.**
  - **`plan`:** lists setting names (`SettingKey`, never values), which are shown before anything runs.
  - **`apply`:** an inherent method on `dyn Configure` (and on `dyn Configure + Send + Sync`), so it can't be overridden. It refuses another companion's plan before sending anything, then calls the implementation's `apply_plan` with a `CheckedPlan` only it can make. `ConfigPlan`'s fields and constructor are crate-private, so a plan can't be forged.
- **`Credentials::get`** returns `Ok(None)` when no credential is stored, and an error when the store failed. A broken store is never read as a missing credential.

## Input types (`inputs`)
Generated compose services and Unraid templates are built only from these validated values:

| Type | Accepts | Refuses |
|---|---|---|
| `HostPath` | Absolute Linux paths with `[A-Za-z0-9._-]` components and inner spaces; at most 255 bytes | See below |
| `Puid`, `Pgid` | `1..=2147483647` | `0` (root) |
| `PortNumber` | `1024..=65535` | Anything lower |
| `ContainerName` | A lower-case letter, then `[a-z0-9_-]`; at most 63 bytes. It's looked up as a host, so it can't contain `.` | IP addresses, decimal addresses, domain names. Also the names a container resolves from its own `/etc/hosts` before Docker's DNS (`localhost*`, `ip6-*`, a container-id-shaped name of 12 or 64 hex digits), and every `lodestar*` name, which Lodestar's own containers use |
| `NetworkName` | Lower-case Docker names | `host`, `none`, `default`, `bridge` |
| `NetworkMode` | A named network, or `service:gluetun` | Anything else |
| `CompanionId` | `[a-z][a-z0-9-]{0,31}`, not ending in `-` | Anything else |
| `Version` | 1–4 dot-separated decimal numbers, each with no leading zero and at most 9 digits; compared number by number | Tags, suffixes, signs, spaces |

**`HostPath` refuses each of these, and every folder that contains one,** since a parent exposes what it holds:
- `/` and the system folders: `/bin`, `/boot`, `/dev`, `/etc`, `/lib*`, `/opt`, `/proc`, `/root`, `/run`, `/sbin`, `/snap`, `/sys`, `/usr`;
- everything under `/var`, except paths strictly below `/var/lib/lodestar/companions` and `/var/spool/lodestar`. That keeps out Lodestar's own data folder, its `secrets.key` and its database, and Docker's state (`/var/lib/docker`);
- `/home` and `/home/<user>`, which hold `~/.docker`, and any dot-folder directly in a home (`~/.ssh`, `~/.local`, `~/.config`);
- under `/mnt`:
  - anything shorter than `/mnt/<pool>/<share>`;
  - the `system` share on any pool, which holds `docker.img`;
  - the `appdata` share itself;
  - `appdata/lodestar` and `appdata/lodestar-worker`, except strictly below their `companions` folder;
- any `*.sock` and any `.docker` folder.

Every comparison ignores case, so a differently cased spelling is refused rather than let through.

**What it can't see:** a symlink into one of these places, or a custom `LODESTAR_DATA_DIR`. Under Docker, Lodestar's own data folder is `/data` inside its container, not the host folder, so `/srv/lodestar`, `/data/lodestar` and `$PWD/data` all pass here. CMP-7 must close that gap (below).

## The CMP-7 contract
These values are **safe once quoted, not bare.**
- **None can hold any of these:** a quote, `\`, a control character, `$`, `:`, `#`, `{`, `}`, `,` or a newline.
- **But some bare values are risky:** a `HostPath` may hold a space, and a name may read as a YAML 1.1 keyword or number (`null`, `true`, `yes`, `on`, `0x10`, `1e3`).

So CMP-7's compose and Unraid generators must:
1. **Quote every YAML key and value** with `serde_json`, and write XML only through `quick-xml`. Never use string concatenation or a hand-written escaper.
2. **Pass values to any program as separate arguments,** never in a shell line. Nothing generated is a shell script.
3. **Refuse a `HostPath` inside Lodestar's own data folder on the host.**
   - **Natively:** check against the running instance's real data folder after `canonicalize`, since `LODESTAR_DATA_DIR` can be anywhere.
   - **Under Docker:** Lodestar sees `/data`, not the host folder. Take the host path from the Unraid template's values or ask the user, and cover Unraid's optional hot and cold data paths too.
   - **Other companions:** refuse a companion's path inside another catalogue companion's `appdata/<name>` folder or host data path. `HostPath` accepts `/mnt/user/appdata/prowlarr`, which is right for Prowlarr, but a qBittorrent container given it could read Prowlarr's `ApiKey`.
4. **Give `TZ` and `UMASK` closed types, or hard-code them.** No free-form value reaches the output.
5. **Golden-test** names and paths that look like YAML 1.1 keywords and numbers, and paths with spaces.
