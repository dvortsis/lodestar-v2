# Legal notice and lawful-use notices (owner request, 2026-11-04, revised the same day)

The owner asked, while reviewing the consent drafts in `docs/decisions/2026-11-02-SRC-4.md`, for language that makes clear Lodestar is for lawful use only, explains the risk, and has the user accept responsibility, so that the owner isn't held liable for what users do. A first version was a fourteen-section Terms of Use. A second review the same day (Codex, at the owner's request) found that a terms-of-use conflicts with the AGPL, and the owner asked for the model BitTorrent clients use. **The result is a notice, not a contract:** `docs/legal/legal-notice.md` (shown once before first use, confirmed as read), `docs/legal/operator-guidance.md` (for anyone who makes an instance public), and the review behind both, `docs/legal/2026-11-04-liability-review.md` (§§6–7 record the second and third reviews). This file amends the "Draft consent texts" section of SRC-4's file without editing it (decisions README: a later file supersedes).

**Not legal advice.** All three reviews were done by AI assistants from secondary sources. The notice and the operator page go to Colorado counsel familiar with open-source licensing before v0.5.1 (owner queue, below).

## Why a notice and not terms (architecture §18.7, the AGPL)
- **The AGPL is the only licence.** Its §9 says nobody has to accept anything to run the program, §7 allows only a short list of additional terms, and §10 forbids further restrictions. Conditions on use (age, conduct, an indemnity, acceptance of future terms) would be further restrictions that recipients may strip, and the project would no longer be AGPL software. So Lodestar follows qBittorrent: licence plus a short legal notice that informs and warns, restates AGPL §§15–16 (as §7(a) allows) and ends on the operator's sole responsibility for what they do with what it finds. No indemnity, no zero-dollar cap.
- **What the notice does, honestly:** it gives conspicuous notice of the network exposure, the nature of the results, the limits of the filters and the operator's responsibility; it is evidence against inducement; and it preserves the AGPL's disclaimers against claims by the operator. It binds no rights holder, regulator or prosecutor. Against them the project's protection is its position in law (*Sony*, *Grokster*, *Cox v. Sony* 2026): a self-hosted, metadata-only, general-purpose tool with real safeguards, not marketed for infringement and not operated by its maker.
- **Every factual sentence in the notice matches the shipped software**, and the prober is marked planned. A PR that changes what Lodestar requests, stores or sends updates the notice in the same PR and raises its version if the change is substantive.

## Where the language goes
1. **The notice ships in the binary** and is shown in full, in the app, without internet access, with its version and date. **A first-run step:** the setup form's password screen gains a required checkbox, unticked by default, "I have read the Legal Notice", with the notice one click away. `POST /setup` refuses a request whose `notice_version` isn't the current one (`409 notice_required`). The confirmation is stored in `settings` under `legal.notice_version` (no time, no author, like consent records; §1.8.8). This is the confirmation qBittorrent asks for at first start, built so a court would recognise it as conspicuous notice (*Berman v. Freedom Financial*, 9th Cir. 2022). It confirms reading; it doesn't condition the licence.
2. **A substantive change re-shows it.** `legal::NOTICE_VERSION` is a compile-time constant; raising it makes the admin's next login show the notice before anything else and asks for the confirmation again. Editorial fixes don't raise it. This mirrors `consent::text_version`.
3. **Settings → About gains a "Legal" section:** the notice (with version), the AGPL-3.0-or-later licence, the brand-assets licence, the operator guidance, and the existing third-party licences list.
4. **The Restricted and Overlay consent screens** keep their own texts and gain a link to the notice, with the closing lines amended below.
5. **Outside the app:** one sentence and a link in the README, in the user guide's chapter 1 (§1.2), and in each Unraid template's `Overview`, so the notice is seen before install.
6. **The private reporting route** the notice names (section 9) needs GitHub's private vulnerability reporting turned on for the repository (owner queue).

**Slices.** The in-app parts (1–4) are one Sonnet-eligible UI slice plus an Opus-owned API change (`POST /setup`, the login gate), to be scheduled by the runner before SRC-15b, since SRC-15b ships the consent screens. The notice body is served as one Markdown document rendered in-app and isn't translated; en and zh message keys hold the UI labels only.

## Amendments to SRC-4's draft consent texts
These apply to the Restricted and Overlay drafts in `docs/decisions/2026-11-02-SRC-4.md`. Each is a change in substance, so `consent::text_version` for the class is raised when SRC-15b adopts the texts (SRC-4's rule). External and Account are unchanged.

- **Restricted:** replace the third bullet and the closing line with:
  > - **You're responsible for whether using it is lawful where you are and for anything you do with what it finds.** Turning it on means you understand this risk and accept it; the [Legal Notice] explains more.
  >
  > Enter your password to turn it on. You'll be asked again if this notice changes.
- **Overlay:** add a bullet before the closing line, and replace the closing line:
  > - **You're responsible for whether joining this network is lawful where you are and for anything you do with what it finds.** Turning it on means you understand this risk and accept it; the [Legal Notice] explains more.
  >
  > Enter your password to turn it on. You'll be asked again if this notice changes.
- **Why only this much.** The consent screens are per §10.14: what happens, then what to do. The consent line ties this specific choice to the operator's responsibility, so a later dispute can't claim the risk wasn't disclosed for this source.

## Standing content rule (refines owner decision §2.28 Q1 and architecture §18.7)
Nothing the project publishes shows or suggests using Lodestar to find commercial films, music, software or books without permission: not the README, screenshots, release notes, fixtures, plugin descriptions, in-app examples or the Unraid templates. Fixture and screenshot titles stay generic or public-domain. This is the *Grokster* / *Fung* evidence rule: marketing is what turned those cases. It's a refinement, recorded under §2.19 and flagged to the owner.

## Owner queue (needs the owner, not decided here)
1. ~~Fill §14.~~ Done 2026-11-04: the owner is in Colorado, so the notice is read under Colorado law with a Colorado forum, and the contact is the public repository. No postal address is included; none is required for free software published by a private individual (review §4). **Open:** whether to name the owner's legal name as publisher, which counsel may prefer; the notice names "the copyright holder who maintains the repository".
2. **Colorado counsel familiar with open-source licensing reviews `docs/legal/legal-notice.md` and `docs/legal/operator-guidance.md` before v0.5.1.** The runner adds this to `owner_queue` in `runner/dashboard.yaml` and to the release gate's conditions.
3. ~~Whether the Anna's Archive and LibGen plugins ship.~~ **Decided 2026-11-04 (owner, §2.37): they don't.** Neither is built or listed. **For the runner:** remove the "Anna's Archive / LibGen importer (restricted consent)" item from `docs/build-plan.md` §0 and the example from the Restricted row of architecture §18.7, and update `runner/dashboard.yaml` if anything there names them. The Restricted class stays for tracker-announce peer finding and any future high-risk source.
4. **Turn on GitHub's private vulnerability reporting** for the repository (Settings → Code security). The notice (section 9) sends security flaws there and nothing else: reports about indexed material go to the reporting authority or the instance's operator, not to the maintainers (owner, 2026-11-04). No `SECURITY.md` contact is needed beyond that.
5. **A DMCA agent** ($6, renewed every three years) only if a public demo, hosted index or user-content site ever exists.

## Shared files touched
- `README.md`: a "Legal" paragraph under "What Lodestar does and never does".
- `docs/legal/`: new folder (the notice, the operator guidance, the review).
- `docs/user/01-before-you-start.md` §1.2, `docs/user/revisions.md` and the regenerated `docs/user/index.md`.
- `packaging/unraid/*.xml`: one sentence in each `Overview`.
