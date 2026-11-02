# B-subscriptions-api decisions (#PR)

The S-blocklists UI needs to list, connect, pause, remove and settle blocklist subscriptions (owner's decision §2.9, architecture §20.13). The change ops (`subscribe`, `unsubscribe`, `pause_subscription`, `resolve_held`) already went through `POST /filters/preview` and `/filters/apply`, so this slice keeps that model, adds the listing the UI needs, and fixes what a security pass over those ops found. The runner decided it under §20 and §2.19. Nothing here loosens filtering without the password and an effect warning. No migration and no schema change.

## Routes (§20.7 "As built", added to the route list)
- **`GET /filters/subscriptions`** (new): the one-click catalog, your lists and their strictness. It stores and connects nothing.
  - `catalog`: each catalog list with `about`, `kind`, `format`, `url`, its `figures` and `subscription` (your list's id once you've connected it, else null). The figures are the catalog's measurement until the list is connected, then its own data.
  - `connected`: your lists, paused ones included, each with its `subscription` entry, its own `figures` and `held` (a version waits for *Accept anyway* or *Keep previous*). `held` reads the stored held version, not the status, because a failed fetch after a held one sets the status to `error` while the version still waits.
  - `strictness`: the enabled lists together, as `POST /filters/strictness` reports it.
- **Changes stay `POST /filters/preview` → `POST /filters/apply`**, one per request:

| Change | Password | Preview reports |
|---|---|---|
| `subscribe {url, kind, format?, action?, name?, floor?, refresh_hours?}` | Only with `floor: true` (it locks entries) | `strictness` after connecting: a catalog list by its measurement (BT_BlockLists comes out severe), any other list unknown until fetched |
| `pause_subscription {id, enabled: false}` | Yes | the list's quarantine and tombstones (`preview.quarantine`, `preview.tombstones`), floor entries leaving (`preview.removed_from_floor`), the warning, and `strictness` without it |
| `pause_subscription {id, enabled: true}` | No | `strictness` with it back |
| `unsubscribe {id}` | Yes | as pausing |
| `resolve_held {id, accept: true, held?}` | When the held version drops anything the applied one blocks, unless the list is floor-marked (§2.7). With the password, `held` must be the preview's `held_version` | `held_version`, `unblocked` (entries that stop being blocked), the holds on the dropped entries, the warning, and `strictness` with the held version |
| `resolve_held {id, accept: false}` | No | nothing changes |

- **`apply` reports `released`** for `unsubscribe`, `pause_subscription` off, and an accepted `resolve_held` that drops entries: the hashes no longer held by anything. They are the holds the preview counted in `quarantine` + `tombstones`, counted again when you apply: ingest can add a hold to a list in between, and a hash another rule also holds stays held and isn't counted as released.
- **`Assessment.strictness`** (new, optional) is set for every subscription change, so the UI can show the strictness warning in the same dialog as the effect warning.

## Fixes and refinements (§2.19: the runner flags these to the owner)
- **A list you remove stops applying at once.** `lists::apply` rebuilt the layers of the lists still subscribed and never cleared a removed list's layer, so the engine kept blocking its hashes (and a floor-marked list's floor entries) until the next start. Releasing its holds then let intake hold them again under the same list. It now drops every layer whose list is gone, before anything is released (§20.13's ordering).
- **Accepting a held version is weakening whenever it drops an entry.** It was judged weakening only when the gate's reason said "shrank", so a version that grew tenfold but dropped entries went through without the password. Now it compares the versions: for a content list, the hashes the applied version has and the held one doesn't; for an IP list, whether the held ranges still cover every applied address (`IpSet::covers`). A held version that doesn't decode counts as blocking nothing.
- **Accepting a version that drops entries releases the list's holds on them, and only those** (`Store::release_rule_hashes`). Before, nothing was released and the preview counted every hold the list had, so the numbers didn't match what happened. §20.13 says narrowing a list releases what it blocked; this follows it. It runs behind the password and the warning's exact counts. Holds by other rules, and on entries the new version still lists, stay.
- **The password confirms the held version the warning described** (found in the fresh Opus review). `resolve_held {id, accept}` used to mean "whatever is held now". The refresh task can hold another version while the warning dialog is open, so the password could apply a version that drops far more. Like `resolve_floor_removal`'s `hashes`, the preview now returns `held_version` (SHA-256 of the held data), and a loosening accept with the password must send it back as `held`. Without it the request is refused with 400 `held_needs_version` before the password is read. `apply` compares it with the held version under the store lock (the refresh task records versions under the same lock) and refuses a mismatch with 409 `changed_since_preview`, as it does an accept that loosens with no password asked for.
- **A floor-marked list says what leaves the floor.** Pausing or removing one takes its entries and its pending removals out of the floor (§2.7), so the warning counts them (`removed_from_floor`, less what CS-1 or another floor list still holds) and says so. It needed the password already. What it already dropped stays dropped: those holds are stored under `CS-1b`, not the list, so they aren't released. That's over-strict, never loose, and unchanged.
- **An unknown list is a 404 before the password is asked for**, in the preview and in apply, for every subscription change.
- **`subscribe` is validated in the preview too,** with the same errors as apply. URLs are http or https, at most 2,048 bytes, with no spaces or control characters; a name is 1–200 characters. Before, the preview accepted anything apply would refuse.
- **Effect warnings name the list** and say what it lets through: torrents fetched and indexed again for a content list, peers and nodes contacted again for an IP list. *Accept anyway* says how many entries (or address ranges) stop being blocked, which the hold counts alone don't show. Each warning offers the safer alternative (pause rather than remove; keep the previous version).
- **Pausing a list that's already paused** still asks for the password. It changes nothing, so this is only over-cautious.

## Defaults (§2.9, §10.10)
- **Nothing is subscribed by default, and no route subscribes implicitly.** Listing, previewing and `POST /filters/strictness` store nothing. Connecting the same URL again returns the existing list, unchanged, so a repeat never resumes a paused list or turns on `floor`.
- **Settings export still leaves subscription state out** (`/settings/export`, §10.10). Nothing in this slice touches it.

## Tests
- `crates/api/tests/filters/subscriptions.rs`, a module of the `filters` test binary: every route's success, validation errors, and a missing or wrong password on each weakening change; released counts equal the preview's (no concurrent ingest in tests); a released hash isn't blocked or held again by the removed list; floor-entry counts; IP-list narrowing; eight concurrent changes to one list (one removal applies, one release); and a property test that no sequence of subscription changes sent without the password unblocks any hash or address.
- A held version swapped after the preview is refused, and nothing is released. An IP list's narrowing, accepted with the password, unblocks only what it dropped, and a held IP version that doesn't decode counts as weakening.
- `IpSet::covers` and `IpSet::uncovered` have a unit test.
- `crates/api/src/log_events.rs` gains `..` in one pattern for the new `held` field; `tests/openapi.rs` lists the new route; the `apply_subscribe` golden gains the `strictness` field.
