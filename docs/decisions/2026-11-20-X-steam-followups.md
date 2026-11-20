# X-steam-followups decisions

Slice X-steam-followups (architecture §18.2, §18.7), the follow-ups from the
review of #969 (ENR-steam). Made by the slice session on 2026-11-20Z for the
runner to confirm. Builds on `2026-11-16-ENR-steam.md`; nothing there is
superseded.

## A suspicious finished list is refused (UX: never lose a working list)
- **What is refused.** A list that finishes downloading is checked against
  the one in use before it replaces it (`steam::suspicious`):
  - an empty list is always refused, the first one included;
  - once the list in use holds at least `SHRINK_FLOOR` (1,000) apps, a list
    with under half as many is refused;
  - anything else is accepted, so the first list is accepted however small,
    and a list under 1,000 apps can shrink freely.
- **Why half, and why 1,000.** Steam lists about 200,000 games and the
  list only grows, by a few percent a month; a real drop of half is not
  plausible, while a cut-off or broken answer (a page of an error body that
  still parses, a filter Valve changed) easily is. Half leaves room for
  Valve pruning delisted games. Under 1,000 apps the list is not Steam's
  real catalogue (tests and odd set-ups), where halving can be ordinary, so
  only an empty list is refused there.
- **Counted as fetched.** Sizes are the apps the parser accepted across all
  pages, before folding, so two games with one name still count twice.
- **What happens.** The list in use stays and keeps answering lookups; the
  partial list is dropped; the next try waits `RETRY` (an hour), as after a
  failed fetch. With no list yet, the lookup is an error carrying the
  warning, as any first-fetch failure is.
- **The warning.** Plain text naming both sizes and saying the list will be
  tried again in an hour, e.g. "Steam's app list came back with 500 games,
  under half the current 1200; the current list is kept and it will be
  tried again in an hour". It's kept until a list is accepted: in memory
  (`Steam::warning`, for tests and the wiring slice) and in the plugin's own
  state store under `steam::WARNING_KEY` (`warning`), the per-source store
  a status page can read. The write is best effort: a failing state store
  loses only the status line, never the list. The `Identifier` trait has
  no status hook today; surfacing the key on the Sources page is left to
  the wiring slice (for the runner).
- **Known limit.** If Steam's list really did halve, the plugin would keep
  the old list and retry hourly until someone looks. That is the safe side
  (old names still match real games); the warning says why.

## Tests for behaviour #969 left untested
- **The 60 s cancel hold** (`CANCELLED`): a lookup cancelled mid-request is
  held until exactly 60 s after the hold was set; at 59 s a lookup is
  `HELD` and sends nothing, at 60 s the refresh runs. Paused tokio clock;
  no sleeps.
- **The partial-list age limit** (`REFRESH`): a partial list one second
  short of 24 h old resumes at its cursor (page one's games kept); one
  exactly 24 h old is dropped and the refresh starts again at page one.

## Not here
- The `ext:` condition for switching the plugin on (the owner's adult
  sign-off, a live re-read of Steam's terms, Valve accepting the
  `x-webapi-key` header) is the runner's, in `runner/dashboard.yaml`.
