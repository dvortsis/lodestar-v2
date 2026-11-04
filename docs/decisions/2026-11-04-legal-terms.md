# Legal terms and lawful-use notices (owner request, 2026-11-04)

The owner asked, while reviewing the consent drafts in `docs/decisions/2026-11-02-SRC-4.md`, for language that makes clear Lodestar is for lawful use only, explains the risk, and has the user accept responsibility, so that the owner isn't held liable for what users do. This file records where that language goes and what it changes. The text itself is `docs/legal/terms-of-use.md`; the review of US federal and state law behind it is `docs/legal/2026-11-04-liability-review.md`. It amends the "Draft consent texts" section of SRC-4's file without editing it (decisions README: a later file supersedes).

**Not legal advice.** The review was done by an AI assistant from secondary sources. The Terms go to a US attorney before v0.5.1 (owner queue, below).

## What the Terms are for, honestly
- **They bind the operator,** who agrees that running an instance, enabling sources and acting on results are their decisions, accepts the risks, and indemnifies the maker. They disclaim warranties and limit liability for claims by the operator, on top of AGPL §§15–16.
- **They don't bind rights holders,** and no text cures inducement (*Columbia Pictures v. Fung*, 9th Cir. 2013, where isoHunt's disclaimers didn't help). Against third parties the project's protection is its position in law: a self-hosted, metadata-only, general-purpose tool with real filtering, not marketed for infringement and not operated by its maker. The design rules below keep it there.

## Where the language goes
1. **The Terms ship in the binary** and are shown in full in the app, without internet access, with their version and date. **A first-run acceptance step:** the setup form's password screen gains a required checkbox, unticked by default, whose label says that ticking it means accepting the Terms, with the Terms one click away. `POST /setup` refuses a request without `terms_version` equal to the current version. The acceptance is stored in `settings` under `legal.terms_version` (no time, no author, like consent records; §1.8.8). This follows the contract-formation rule the Ninth Circuit applies: conspicuous notice plus an unambiguous act of assent that the user is told is assent (*Berman v. Freedom Financial*, 2022; *Chabolla v. ClassPass*, 2025).
2. **A substantive change re-asks.** `legal::TERMS_VERSION` is a compile-time constant; raising it makes the admin's next login show the Terms before anything else, and refuses the session until they're accepted. Editorial fixes don't raise it. This mirrors `consent::text_version`.
3. **Settings → About gains a "Legal" section:** the Terms (with version), the AGPL-3.0 licence, and the existing third-party licences list.
4. **The Restricted and Overlay consent screens** keep their own texts and gain a link to the Terms, with the closing lines amended below.
5. **Outside the app:** one sentence and a link in the README, in the user guide's chapter 1 (§1.2), and in each Unraid template's `Overview`, so the notice is seen before install. These add notice; the in-app checkbox is what forms the contract.
6. **Terms §14 is filled** (Colorado; the public repository as contact), so the first-run step can be built.

**Slices.** The in-app parts (1–4) are one Sonnet-eligible UI slice plus an Opus-owned API change (`POST /setup`, the login gate), to be scheduled by the runner before SRC-15b, since SRC-15b ships the consent screens. The texts are owner-approved copy, so the slice doesn't rewrite them; en and zh message keys hold the UI labels only, and the Terms body is served as one Markdown document rendered in-app.

## Amendments to SRC-4's draft consent texts
These apply to the Restricted and Overlay drafts in `docs/decisions/2026-11-02-SRC-4.md`. Each is a change in substance, so `consent::text_version` for the class is raised when SRC-15b adopts the texts (SRC-4's rule). External and Account are unchanged.

- **Restricted:** replace the third bullet and the closing line with:
  > - **You're responsible for whether using it is lawful where you are and for anything you do with what it finds.** Turning it on means you understand this risk and accept it, as the [Terms of Use] say.
  >
  > Enter your password to turn it on. You'll be asked again if this notice or the Terms change.
- **Overlay:** add a bullet before the closing line, and replace the closing line:
  > - **You're responsible for whether joining this network is lawful where you are and for anything you do with what it finds.** Turning it on means you understand this risk and accept it, as the [Terms of Use] say.
  >
  > Enter your password to turn it on. You'll be asked again if this notice or the Terms change.
- **Why only this much.** The consent screens are per §10.14: what happens, then what to do. The legal weight sits in the Terms, which the operator has already accepted; the consent line ties this specific choice back to them so a later dispute can't claim the risk wasn't disclosed for this source.

## Standing content rule (refines owner decision §2.28 Q1 and architecture §18.7)
Nothing the project publishes shows or suggests using Lodestar to find commercial films, music, software or books without permission: not the README, screenshots, release notes, fixtures, plugin descriptions, in-app examples or the Unraid templates. Fixture and screenshot titles stay generic or public-domain. This is the *Grokster* / *Fung* evidence rule: marketing is what turned those cases. It's a refinement, recorded under §2.19 and flagged to the owner.

## Owner queue (needs the owner, not decided here)
1. ~~Fill Terms §14.~~ Done 2026-11-04: the owner is in Colorado, so Colorado law governs, and the contact is the public repository. No postal address is included; none is required for free software published by a private individual (review §4), and keeping one out of a public repository is the safer choice.
2. **Attorney review of `docs/legal/terms-of-use.md` before v0.5.1.** The runner adds this to `owner_queue` in `runner/dashboard.yaml` and to the release gate's conditions.
3. **Whether the Anna's Archive and LibGen plugins ship at all.** Both sites are under US default judgments and broad injunctions (LibGen, S.D.N.Y. 2024; Anna's Archive, S.D.N.Y. and S.D. Ohio, 2026). Bundling plugins for them is the largest inducement signal a plaintiff could point to, and no disclaimer offsets it. The review recommends dropping them or shipping them unlisted (a user supplies the manifest). This changes scope in `docs/plan/2026-11-01-v06-sources.md`, so it's the owner's call.
4. **A DMCA agent** ($6, renewed every three years) only if a public demo, hosted index or user-content site ever exists.

## Shared files touched
- `README.md`: a "Legal" paragraph under "What Lodestar does and never does".
- `docs/user/01-before-you-start.md` §1.2, `docs/user/revisions.md` and the regenerated `docs/user/index.md`.
- `packaging/unraid/*.xml`: one sentence in each `Overview`.
