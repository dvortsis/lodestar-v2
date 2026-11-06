# lodestar-companions: the companion manager core

The start of Lodestar's companion manager (architecture §13, §18.10; plan `docs/plan/2026-11-06-v06-companions.md`). This crate holds the adapter traits, the closed input types and the shared identifiers. The compiled-in catalogue comes with CMP-0. Fetching, extraction, credentials, supervision and the API arrive in later CMP slices, each as its own module.

**This file is the contract** every later CMP slice reads.

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
  - **Paths are `RequestPath`s:** RFC 3986 characters, no fragment, and no `//`, `.` or `..` segment, including the `%2e` forms.
  - **Headers are `HeaderName` constants with checked values.** A credential stays a `Secret` until the client writes it. `Debug` never shows a header value.
- **`EnteredUrl` checks shape only:**
  - `http` or `https`;
  - RFC 3986 characters only, refusing `\` and `"<>{}|^` and `` ` ``;
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
  - **`apply`:** refuses another companion's plan before sending anything, then calls the implementation's `apply_plan`. That changes nothing the plan doesn't list.
- **`Credentials::get`** returns `Ok(None)` when no credential is stored, and an error when the store failed. A broken store is never read as a missing credential.

## Input types (`inputs`)
Generated compose services and Unraid templates are built only from these validated values:

| Type | Accepts | Refuses |
|---|---|---|
| `HostPath` | Absolute Linux paths with `[A-Za-z0-9._-]` components and inner spaces; at most 255 bytes | See below |
| `Puid`, `Pgid` | `1..=2147483647` | `0` (root) |
| `PortNumber` | `1024..=65535` | Anything lower |
| `ContainerName` | A lower-case letter, then `[a-z0-9_-]`; at most 63 bytes. It's looked up as a host, so it can't contain `.` | IP addresses, decimal addresses, domain names, anything else |
| `NetworkName` | Lower-case Docker names | `host`, `none`, `default`, `bridge` |
| `NetworkMode` | A named network, or `service:gluetun` | Anything else |
| `CompanionId` | `[a-z][a-z0-9-]{0,31}`, not ending in `-` | Anything else |
| `Version` | 1–4 dot-separated decimal numbers, each with no leading zero and at most 9 digits; compared number by number | Tags, suffixes, signs, spaces |

**`HostPath` refuses each of these, and every folder that contains one,** since a parent exposes what it holds:
- `/` and the system folders: `/bin`, `/boot`, `/dev`, `/etc`, `/lib*`, `/opt`, `/proc`, `/root`, `/run`, `/sbin`, `/snap`, `/sys`, `/usr`;
- everything under `/var`, except paths strictly below `/var/lib/lodestar/companions` and `/var/spool/lodestar`. That keeps out Lodestar's own data folder, its `secrets.key` and its database, and Docker's state (`/var/lib/docker`);
- `/home` and `/home/<user>`, which hold `~/.docker`;
- under `/mnt`:
  - anything shorter than `/mnt/<pool>/<share>`;
  - the `system` share on any pool, which holds `docker.img`;
  - the `appdata` share itself;
  - `appdata/lodestar` and `appdata/lodestar-worker`, except strictly below their `companions` folder;
- any `*.sock` and any `.docker` folder.

**What it can't see:** a symlink into one of these places, or a custom `LODESTAR_DATA_DIR`. CMP-7 also checks against the running instance's real data folder (below).

## The CMP-7 contract
These values are **safe once quoted, not bare.**
- **None can hold any of these:** a quote, `\`, a control character, `$`, `:`, `#`, `{`, `}`, `,` or a newline.
- **But some bare values are risky:** a `HostPath` may hold a space, and a name may read as a YAML 1.1 keyword or number (`null`, `true`, `yes`, `on`, `0x10`, `1e3`).

So CMP-7's compose and Unraid generators must:
1. **Quote every YAML key and value** with `serde_json`, and write XML only through `quick-xml`. Never use string concatenation or a hand-written escaper.
2. **Pass values to any program as separate arguments,** never in a shell line. Nothing generated is a shell script.
3. **Refuse a `HostPath` inside the running instance's data folder,** checked after `canonicalize` where it's on the same machine, since `LODESTAR_DATA_DIR` can be anywhere.
4. **Give `TZ` and `UMASK` closed types, or hard-code them.** No free-form value reaches the output.
5. **Golden-test** names and paths that look like YAML 1.1 keywords and numbers, and paths with spaces.
