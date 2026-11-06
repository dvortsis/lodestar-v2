# lodestar-companions: the companion manager core

The start of Lodestar's companion manager (architecture §13, §18.10; plan `docs/plan/2026-11-06-v06-companions.md`). This crate holds the adapter traits, the closed input types and the shared identifiers. The compiled-in catalogue comes with CMP-0. Fetching, extraction, credentials, supervision and the API arrive in later CMP slices, each as its own module.

**This file is the contract** every later CMP slice reads.

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
| `HostPath` | Absolute Linux paths with `[A-Za-z0-9._ -]` components; at most 255 bytes | `/`; `/etc`, `/proc`, `/run`, `/opt`, `/var` and the other system folders (under `/var`, only `/var/lib/lodestar` and `/var/spool/lodestar` are allowed); any `*.sock`; any `.docker` folder; Unraid's `/mnt/<pool>/system` |
| `Puid`, `Pgid` | `1..=2147483647` | `0` (root) |
| `PortNumber` | `1024..=65535` | Anything lower |
| `ContainerName` | Lower-case Docker names | Anything else |
| `NetworkName` | Lower-case Docker names | `host`, `none`, `default`, `bridge` |
| `NetworkMode` | A named network, or `service:gluetun` | Anything else |
| `CompanionId` | `[a-z][a-z0-9-]{0,31}`, not ending in `-` | Anything else |
| `Version` | 1–4 dot-separated decimal numbers, each with no leading zero and at most 9 digits; compared number by number | Tags, suffixes, signs, spaces |

None of them can hold a character that means something to YAML, compose interpolation (`$`), volume syntax (`:`), XML or a shell. CMP-7 still quotes every value it writes.
