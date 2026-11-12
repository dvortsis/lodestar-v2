# Runner decisions: tokio 1.54.1 and the notice's "Applies to" line on `release/v0.5.2`

- **tokio 1.54.0 → 1.54.1 on `release/v0.5.2`** (owner, 2026-11-12: "go with the runner's recommendation"; recorded as `docs/owner-decisions.md` §2.58 on `main`).
  - **Why:** tokio 1.54.0 can drop a timer entry the time driver is still writing to when `Interval::poll_tick` resets inside `select!` (tokio-rs/tokio#8695; fixed by tokio-rs/tokio#8692 in 1.54.1). crates.io has yanked 1.54.0. It aborted a DHT test on `main` (X-flake-dht-scrape-join, #731). Every crate using `tokio::time::interval` carried the race, so v0.5.2 must not ship on 1.54.0.
  - **The change is the same as #731's on `main`:**
    - `Cargo.lock`: tokio's version and checksum only.
    - Every `tokio` requirement on 1.54.0 now asks for 1.54.1. `crates/companions` keeps an exact pin, now `=1.54.1`. No features change.
    - `web/licenses/rust.json`, regenerated with `.github/scripts/licenses.py`.
  - **`fuzz/Cargo.lock`** gets the same bump. cargo also refreshes the workspace crates' own versions there from 0.5.0 to 0.5.2, which the cut had left stale. No third-party package changes version.
  - **No v0.5.1.x now:** v0.5.2 carries the fix. If v0.5.2 slips by more than a few days, the runner prepares a v0.5.1.1 with only this bump and asks the owner before anything is cut or tagged (§2.58).
- **The Legal Notice's "Applies to" line on this branch names 0.5.2 and its release candidates only** (owner delegation, `docs/owner-decisions.md` §2.57; the mirror image of #725 on `main`).
  - The line said version 4 applied to "the development builds that lead to it (the `main` branch)". `main` now carries version 5, so that was wrong.
  - It now reads: "Applies to Lodestar 0.5.2 and its release candidates. … Each release carries its own copy of this notice, written for what that build does: v0.5.1 carries version 3, and the development builds on the `main` branch carry a later version."
  - The bracketed note "a development build may report an earlier version number" is dropped. It only fit `main`'s builds, and this branch's builds report 0.5.2.
  - `docs/legal/operator-guidance.md`'s status line gets the same change. Its date stays, because the substance it describes is unchanged.
  - **`NOTICE_VERSION` stays 4, and so does its date.** The edit says which software the notice describes, not what that software does, so it isn't a change in substance. The precedent is 9cf2998d and #725. Both notice copies stay byte-identical (`crates/lodestar/tests/legal_facts.rs`).
