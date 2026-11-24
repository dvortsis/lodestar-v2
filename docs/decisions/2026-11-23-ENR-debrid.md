# ENR-debrid decisions (#1103): debrid cache status skipped, no code ships

Slice ENR-debrid (`runner/slices.yaml`). Spec: architecture §18.2 and §18.7, plan `docs/plan/2026-11-05-v07-m7.md` §6 Q4 and Q5, decided by `docs/owner-decisions.md` §2.47 ("the plan row is the decision").
- **The decision, plan §6 Q4's row** (decider: "Owner (ToS, per §2.28's rule)"): "Each slice records its check; the four grey ones launch only with the owner's yes. Recommend yes for AniDB (registered client, ≤ 1 request per 2 s, interest-only), srrDB and PreDB (≤ 1/s, interest first); debrid only for a provider that offers a cache check today, otherwise drop it from v0.7".
- **§2.47's summary bullet of it:** "Debrid: only for a provider that offers a cache check today and whose terms allow it, otherwise dropped from v0.7. Its hash lookups of verified public items are allowed (v0.7 Q5's exception)."

## Decision: no provider qualifies, so debrid cache status is dropped from v0.7
- **The test applied (Q4, and the runner's note for this slice):** a provider qualifies only if (a) its current, official API documentation offers a cache or instant-availability check by infohash, **and** (b) its terms clearly allow a third-party app to make those lookups automatically with the user's own key. Where either is unclear, the provider doesn't qualify.
- **A refinement (§2.19), the runner's call:** "clearly allow, and unclear means it doesn't qualify" is stricter than Q4's "whose terms allow it", and stricter than ENR-srrdb's bar ("no published term forbids it"). Debrid gets the stricter bar because Q4's condition is phrased positively (the terms must allow it, not merely not forbid it), and because these lookups send infohashes, which the other enrichment services never receive. It's stricter only, so it loosens nothing.
- **Result:** none of the five providers checked passes both. Two (Real-Debrid, AllDebrid) no longer document a cache check; Debrid-Link's docs couldn't be read, and community reports say its check was removed. The two that still have one (Premiumize, TorBox) have terms this check couldn't read from the provider itself. So, per Q4, **this slice ships no code** and debrid status is out of v0.7. This applies the owner's decision as written; it doesn't loosen or change it.
- **Nothing was sent to any provider's API.** Only public documentation, changelog and terms pages, plus web searches, were read. No account, key or contact detail was used.

## The provider check, 2026-11-23 (UTC)
| Provider | (a) Cache check by hash today | (b) Terms allow it | Qualifies |
|---|---|---|---|
| Real-Debrid | **No.** The current API reference (`https://api.real-debrid.com/`) lists no `/torrents/instantAvailability`; it was disabled in late 2024. | Not reached | No |
| AllDebrid | **No.** The current docs (`https://docs.alldebrid.com/`) list no instant endpoint. The only signal is the `ready` field on a magnet **upload**, which adds the magnet to the user's account (30 active at most): an action, not a lookup. | Not reached | No |
| Debrid-Link | **Not shown.** The official API docs (`https://debrid-link.com/api_doc/v2/introduction`) rendered no endpoint list here. Community reports say Debrid-Link removed its availability check in late 2024 alongside Real-Debrid | Not reached | No |
| Premiumize | **Partly.** `POST /api/cache/check` exists (`https://www.premiumize.me/api`), but the official docs describe `items[]` as "Links to check", not hashes. An undated Premiumize blog post says it takes a link, file hash or magnet and costs no fair-use points | **Unclear.** The terms couldn't be read from premiumize.me (`/terms`, `/terms-of-service` returned 404, `/legal` didn't answer). Only a third-party copy (dated 2025-07-30) was found; it bars bot-registered accounts and says nothing about API lookups | No |
| TorBox | **Yes, by third-party sources.** `GET /v1/api/torrents/checkcached?hash=<h1,h2,…>`, about 100 hashes a request, per a docs mirror (`https://developers.qodex.ai/torbox/main-api/torrents/get-torrent-cached-availability`) and an SDK; TorBox's changelog (`https://torbox.featurebase.app/en/changelog/v50`, 2024-11-10) says the endpoint stays. The official docs (`https://api-docs.torbox.app/`) are script-rendered and couldn't be read here | **Unclear.** The terms on torbox.app are script-rendered and couldn't be read. A third-party report says they were rewritten on 2026-07-31; TorBox's May 2026 changelog says it removed "users who continue to abuse our systems", and the abuse-system article it points to wasn't found. No published text found either allows or forbids automated cache checks by a third-party app | No |

## What would bring it back (for the runner)
- **TorBox is the nearest candidate,** then Premiumize. If a person reads TorBox's current terms (and its abuse-system article) in a browser and finds they clearly allow a third-party app to check hashes with the user's own key, the runner can relaunch ENR-debrid for TorBox under the same Q4 answer, with a new decisions file that supersedes this one. That's a re-check of facts, not a new owner question: Q4 already says yes for a provider that qualifies.
- **Two caveats on a relaunch:**
  - If a re-read finds the terms silent or ambiguous, as TorBox's are today, that's a grey case and goes to the owner under §2.28. It isn't the runner's call.
  - A relaunch under this Q4 answer covers v0.7 only. After v0.7, debrid is a question for the next plan.
- **The slice's other limits stay as they are for any relaunch:**
  - Account consent, and the user's own key (never logged or stored outside `SecretStore`).
  - Only verified public items' hashes: never `private_source`, held, hidden, unverified or Adult. Adult items go only to ThePornDB and StashDB (Q5); debrid's exception lifts only Q5's "never the hash" rule, and only for verified public items.
  - The provider's own rate limit.
  - From the Account class (§18.7): credentials are never exported, and results are local-only, so they never appear in the API, exports, Torznab or Stremio.
  - From Q5: user interest first, then a capped backfill, and nothing is sent until the plugin is enabled with its consent.
  - The only output is a "Cached on <service>" badge: an icon plus text, in one slot, shown only with an account.
- **The runner marks the slice and its plan row** (`runner/slices.yaml`, `runner/dashboard.yaml`, the v0.7 plan) as skipped; this PR doesn't edit them, as they're the runner's files.
- **The badge follow-up (UI review 02 §5.5) isn't needed** while no provider ships.
