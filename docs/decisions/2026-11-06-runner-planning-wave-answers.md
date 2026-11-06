# Runner: the planning-wave questions, decided on performance and UX

Runner, 2026-11-06. The owner answered owner-queue item 10, the planning-wave question list, at 2026-11-06T04:26Z, relayed by the dashboard session: "Have the runner decide based on what's best for performance and UX. Provide a summary of the decisions." The record is `docs/owner-decisions.md` §2.46. This file gives each question's decision, its reason, and what it unblocks.

**How the runner read the delegation.** It covers the questions whose weight is performance, UX or engineering practice, where the plan's recommendation loosens nothing. CLAUDE.md keeps some decisions with the owner even under a delegation: loosening an owner decision, weakening security or content safety, a source's terms or consent, and changing an owner-set number other than for performance and UX under §2.26. Questions of those kinds aren't treated as covered. They're listed by name below, and the dashboard session asks the owner to confirm each one.

## Decided (the plan's recommendation in each case; the condition holds)
| Question | Decision | Reason | Unblocks |
|---|---|---|---|
| v0.7 Q5, what general enrichment may send (`owner-v07-q5`) | Only verified items, and only the parsed title, year and type. Never held, hidden, `private_source`, unverified or Adult items. Adult goes only to ThePornDB and StashDB, under §2.37's rule-pack Q2. | It's the narrowest rule that still lets enrichment work, and it's the same rule the owner already set for adult enrichment (§2.37 Q2), so it loosens nothing. One rule for every enrichment source keeps ENR-10's eligibility check single and testable. | ENR-10 and every enrichment slice after it |
| v0.7 Q6, Usenet visibility (`owner-v07-q6`) | Hidden by default, as now. Usenet items show only with `network:usenet` or `verified:no`, with the unverified badge. | Unverified Usenet entries would otherwise dilute torrent results. Keeping today's default changes nothing for existing users, and the filter makes them one click away. | UN-3 |
| Companions cq1, the catalogue's trust root (`owner-cq1`) | Compiled into the binary, so the release signature covers it and it changes only with a release. | It's the strictest option and adds no new key or update channel to run. | CMP-0 |
| Companions cq2, upstream signatures (`owner-cq2`) | The SHA-256 pin is always the run-time gate. Upstream OpenPGP signatures are checked once, when a pin is made. | Install time needs no keyring or network fetch at run time, so it's faster and has fewer failure modes. The pin is at least as strict as a signature check. | CMP-1, CMP-13 |
| Companions cq4, qBittorrent on Windows (`owner-cq4`) | A guided install (option a). The service is never elevated. | It's the only option that needs no elevated rights, and qBittorrent's own installer is the path its users know. | CMP-11 |
| Companions Q20, the systemd unit's hardening (`owner-cq7`) | No relaxation by default. CMP-G's smoke test lists exactly what fails under the shipped unit, and the runner brings each item back to the owner with a recommendation. | Nothing is loosened now, and any later relaxation goes to the owner item by item, with evidence. | CMP-4b, CMP-G |
| Workers wkq1, what a worker holds and does (`owner-wkq1`) | Drop (hash-list matches) happens on the worker. Flag, demote, hide, quarantine and the adult hold are only marked there, and the coordinator applies them at commit. Sealed names sit in the worker's spool while it's disconnected, up to 1 GB or 24 h. | The strongest action, drop, happens at the edge with no delay. The others stay reviewable and correctable in one place, and the worker needs less state and memory. Spooled names are sealed, and the spool is bounded. Content safety isn't weakened. | WK-8 |
| Workers wkq3, worker RAM at about 40–50M (`owner-wkq3`) | A separate, smaller worker filter. It keeps the owner's 256 MB flat to 50M (about 246 MB), at about a 2% false-positive rate, which costs only a lookup. | It keeps the owner's 256 MB number rather than raising it. A false positive costs one extra coordinator lookup and never a wrong result. | WK-3, WK-G |
| M8 OQ2, relevance on real data (`owner-m8-q2`) | A committed set built only from openly licensed names, plus a held-out set judged on the import host, with only aggregates committed. | Real-data relevance is measured, but nothing that isn't openly licensed enters the repository. | M8-REL-2 |
| M8 OQ3, the Windows soak for 1.0 (`owner-m8-q3`) | CR-0's 24 h rule, in MSI service mode, on the RC. | It's the same rule as v0.5.1, so results compare, and service mode is how users run it. Naming the host stays the owner's (`windows-soak-host`). | M8-WIN-1 |
| M8 OQ4, a Linux Docker soak on the RC (`owner-m8-q4`) | Yes, 24 h on Unraid. | 1.0 ships the Docker image, and r3–r5 showed memory regressions show up only in a soak. The host stays the owner's (`unraid-soak-host`). | G-v1-soak-linux |
| M8 OQ5, a second-vendor security pass | Optional, on SEC-1 and SEC-2, run by the owner if wanted. It gates nothing. | No condition or slice waits on it. | — |
| M6 Q10, Windows Restart now | Lodestar keeps its own count and offers Restart now only when no recovery was used in the last 24 h. | A crash always has an automatic recovery left. It's the approved design (§10.10.6), with no new grant. If the owner approves Q13's helper, a later change can lift the limit. | PK-win-restart (already ready on the soak verdict) |
| v0.8 Q3, per-worker consent wording | The runner drafts it: today's text plus which machine's IP the requests leave from. **Approving the text stays with the owner**, as for the Q1 consent texts, so `owner-v08-q3` stays unmet. | Consent text is the owner's to approve (§2.37's Q1 item). | PL-7b, once the text is approved |

## Not covered by this reply (the owner confirms each one by name)
| Question | Why it stays with the owner | The plan's recommendation, offered again |
|---|---|---|
| v0.7 Q1, the Stremio addon | A token in the URL path and `Access-Control-Allow-Origin: *` loosen the API's auth rules. Shipping it at all is an inducement question. | Off by default. A dedicated read-only, revocable `stremio` key, redacted from logs. CORS only on the addon paths. LAN only recommended. Every filter and pack applies. |
| v0.7 Q2, private-tracker IRC announces | A source's terms | Ship off, with Account consent and `private_source`. No shipped definition for a tracker whose rules forbid bots. |
| v0.7 Q3, PreDB's live IRC feed | A source's terms | Ship a network only if its operators say read-only clients are fine. Otherwise the user enters their own. |
| v0.7 Q4, AniDB, srrDB, PreDB and debrid | A source's terms | Yes for AniDB (at most 1 request every 2 s), srrDB and PreDB (at most 1 a second). Debrid only with a cache check today. |
| Companions cq3, least privilege on native installs | Running a companion under Lodestar's own account lets it read Lodestar's secrets. | The same account, sandboxed (Landlock; a restricted token on Windows). A plain warning where no sandbox applies. Never root or administrator. |
| Companions cq5, revealing a generated credential | It adds a way to show a secret. | Reveal after the password is re-entered, as a logged event. Never in exports, backups or support bundles. |
| Companions cq6, whose binaries run | Trust in third-party builds | userdocs static `qbittorrent-nox`; Prowlarr from upstream; linuxserver.io images over hotio; gluetun and FlareSolverr from their projects. All pinned. |
| Companions Q21, FlareSolverr | It works around sites' bot protection, which is a terms question. | Opt-in only, off and not installed by default. |
| M8 OQ1, a self-hosted bench runner on a public repo | Security | Mitigations (a)–(d): a `release/*` ruleset, `runner-guard`, fork approval, and main and `release/*` only. |
| M8 OQ6, disclosure and `SECURITY.md` | Security policy, and the owner's own contact | Private vulnerability reporting, a named contact, unfixed findings listed publicly by id and severity only. |
| M6 Q13, how the MSI updater gets install rights | A privileged helper is a new elevated surface. | The helper, with its six rules. If declined, MSI installs stay by hand. |
| Workers wkq2, the join token in Docker and Unraid | It loosens a security decision. | Accept `LODESTAR_TOKEN` only for the first join, then ignore it, warn, and never log it. The token is single-use, bound to a name and expires in 15 min. |
| v0.8 Q1, Nostr relays and Lemmy instances | A source's terms | Public events only. Relays that need auth or payment show "blocked by source". |
| v0.8 Q4, third-party adult claims | Content safety | A claim may only hide an item until local detection runs. It never feeds the child-safety tombstone path and never moves a title into Adult on its own. |

## Not needed yet
- **v0.7 Q10(d).** Steps (a)–(c) shrink the producers first. They're the runner's call under §2.26, and GS-gate makes an over-budget producer turn CI red. Step (d) turns trigrams off by default, which trades the owner's "Regex < 500 ms" for the 512 B budget. That's one owner number for another, so if (a)–(c) fail it goes back to the owner by name, with the measured numbers.

## The owner's own steps (unchanged)
- M6 Q6: winget, submitted after 1.0 if the owner wants it.
- M6 Q8: the nightly signing key (owner-queue item 5).
- M6 Q14: the G-m6 manual installs and the release approval (`owner-m6-manual`).

## Dashboard
- `runner/dashboard.yaml`: the eleven conditions in the first table that have one now have `met: true`. Owner-queue item 10 now lists only the questions not covered.
- `runner/slices.yaml`: each of those external entries is marked as answered by §2.46 and as holding.
- Every slice those conditions gate also waits on `ext:v051-soak-verdict`, so nothing becomes ready to launch now.
