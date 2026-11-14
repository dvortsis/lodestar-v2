# Runner: owner-queue decisions (#819)

On 2026-11-13 the owner gave the runner owner-queue items 4, 10, 14, 16, 17, 18, 19, 21 and 22: "Decisions: Everything except filters - Approve Agent Runner making the best decisions." (`docs/owner-decisions.md` §2.69). Items 13 (FILTER-01) and 15 (FILTER-03) stay with the owner.

These are the runner's answers. Each is a §2.19 call, flagged to the owner, who can veto it. None reverses or loosens an owner decision, weakens a security or content-safety decision, or changes a number the owner set. Held findings are named by id, severity and surface only (§2.47, `docs/security/v1/README.md` §5).

## The owner queue
- **Item 4, Checkpoint 1:** if the projection falls short of the crawler gate, the runner first runs the Track C fixes and measures again.
  - If it is still short, the runner reports to the owner with the evidence, and the owner re-decides the number. The runner never loosens the gate (§2.27); this delegation doesn't change that.
  - `ext:owner-checkpoint-1` now holds: the rule for the checkpoint is decided ahead of it.
- **Item 10, PK-nightly-host:** the proposal in `docs/decisions/2026-11-08-PK-manifest.md` is approved as written.
  - It applies only if the Nightly channel is offered.
  - It pairs with owner item 3, the nightly signing key, which stays with the owner (§2.52).
- **Item 14, APIFEAT-01 (blocking, rules):** the runner chose between the two held options. The runner gives the fix session the choice directly (`docs/security/v1/README.md` §5 step 4); no file holds it.
- **Item 16, INFRA-01: yes.** The image publish waits for the same `release` environment approval as the binaries.
  - On `main` this is a fix slice. For v0.5.2 it's a release PR to `release/v0.5.2` before the tag.
  - A workflow change that only adds a gate is a tightening.
- **Item 17, the UI review's eight part 4 proposals** (build-plan §0.3 row 17): seven are accepted, in part 4's order, as Track U step 6. Each was checked against the owner's UI decisions (§1.5, §2.9, §2.13, §2.14, §2.37, §2.39, §2.51, §2.66) and architecture §10.
  1. **Name the app's intended reader in §10.0:** accepted. It matches the reader the owner set for the guide (§2.14).
  2. **Reorder Search around words and clicks, with new default facets and row facts:** accepted. The new defaults stay within §2.37 Q4 (no performer facet) and §2.66 (versions grouped, no third-party images).
  3. **A plain health summary ahead of the Insights story:** accepted. The story the owner asked for (§1.1, item 7) stays after it.
  4. **Three required setup steps and a checklist in place of nine inline steps:** not accepted: conflicts with §2.13, §2.39 and §2.9. The first-run wizard asks the update choice (§2.13), shows every rule pack checked for review (§2.39) and offers the blocklist subscriptions (§2.9). Track U step 6 leaves out "a checklist in place of the nine-step wizard".
  5. **Lead the network step with safety, not port forwarding:** accepted. It's the safe choice §2.14 asks the guide to recommend, and the experiment host runs that way (§2.51).
  6. **Units: accept GB as typed.** Accepted, rather than a units preference.
  7. **"Alert" as the single noun:** accepted. It changes `docs/ui-glossary.md`.
  8. **Single-key actions that launch another program: a first-use prompt.** Accepted, rather than default off. Single-key shortcuts can still be turned off or remapped (§10.13).
  - `ext:owner-ui-part4` now holds.
- **Item 18, the v0.6 tag scope:** the tag waits on the crawler gate only (P4), as the plan already says. WK-G, CMP-G and G-m6 gate their features' defaults, not the tag.
- **Item 19: yes.** Track Q's hostile-input fixes (PIPE-01, DHT-01, SEARCH-01, STORE-02, PIPE-02) go ahead of ladder rungs 2 and 3.
- **Items 21 and 22: the Legal Notice §2 sentences are approved as proposed.** The runner checked both against the code on `main` (667479cb).
  - **Torrent cache (item 21), approved:** "The torrent cache stays off until you turn it on in Settings → Sources and accept the list of websites it will contact. If that list changes, or the consent text is updated, it turns itself off and asks you again." From `docs/decisions/2026-11-12-X-consent-gate-lift.md`.
    - **The code does this.** An External acknowledgement is bound to the text version and the full address list (`consent::version`). A different list or text no longer matches, so the next start switches the source off with `consent_lapsed` (`control::switch_off_unapproved`), and turning it back on asks for the consent again.
    - **When:** a list edited in Settings applies at the next start, and is switched off there. The sentence doesn't say "at once", so it holds.
  - **Account (item 22), approved, subject to X-consent-gate-lift-account making it true:** "Prowlarr or Jackett stays off until you turn it on in Settings → Sources, accept what an Account source does, and enter its API key; you confirm with your admin password." Proposed 2026-11-13T03:41Z.
    - **What the code does now:** the server asks for the admin password on every Account enable (`consent::needs_password`, X-consent-gate-lift r2), and asks for the declared API key (`consent::need_with`). The Account dialog shows the §2.49 Account text and the key fields.
    - **What it doesn't do yet:** the Account dialog has no password field (`needsPassword` in `web/src/lib/features/sources/consent.ts` leaves out Account), so the user can't confirm with the password there. X-consent-gate-lift-account adds that field, as r2's decisions file says. The sentence ships only with it.
  - `ext:owner-notice-torrent-cache` and `ext:owner-notice-account-wording` now hold, so X-consent-gate-lift-external and X-consent-gate-lift-account are unblocked. SRC-02 is still settled before either flip, as build-plan §0.3's veto list says.

- **Item 12, the status connector deploy:** done (the connector is deployed), so the item leaves the queue.

## Other runner calls (§2.19)
- **The gluetun host file lives outside Lodestar's data folder.** Nothing from Lodestar's data folder is ever mounted into a companion. X-gluetun-templates-hostfile writes `post-rules.txt` to a separate companion-files folder and mounts that one. Why: the data folder holds the store, keys and credentials, and a companion has no reason to see any of it.
- **X-ci-advisories' triage of the three open advisories:**
  - **ctutils:** bumped through the lockfile.
  - **rustix:** pinned to `=1.1.6`.
  - **libsqlite3-sys (RUSTSEC-2026-0459):** a dated 30-day ignore in `deny.toml`, reviewed by 2026-12-13. The mitigation meanwhile is the importer's refusal of a database with `-wal` or `-journal` files. The new slice X-rusqlite-041 (rusqlite 0.41, libsqlite3-sys 0.39) fixes it and removes the ignore.

## Slices added to `runner/slices.yaml`
- **X-sources-nits** (helper): #816's optional review findings (runner-state brief).
- **X-gs2-nits** (Opus): rv-804's O1 to O5 for GS-2 (#804).
- **X-gluetun-hostfile-ui** (helper): the companions screen's field for `gluetun_paths`, after X-gluetun-templates-hostfile.
- **X-rusqlite-041** (Opus): the upgrade above, after X-ci-advisories. `/security-review`, because the magnetico importer opens databases the user supplies.
- **X-test-vm:** the cloud tester's coordinator session (owner-started, §2.60), so its slot has an entry.
- **Notes:** X-gluetun-templates-hostfile gets the separate folder and the files the change needs; rv-815 O6 (the gluetun pin test's change filter) was first given to X-ci-advisories, which did not take it, and is now its own slice, X-gluetun-pin-ci-filter (#822); X-consent-gate-lift-account gets the `setupNeed` revisit.

## Flags for the owner (§2.19)
- Every item above. Item 17's proposal 4 is the one not accepted, because it conflicts with three of your decisions.
