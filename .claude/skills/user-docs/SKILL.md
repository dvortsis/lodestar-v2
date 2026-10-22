---
name: user-docs
description: Lodestar's rules for user-facing documentation, layered on the owner's documentation-skills (github-docs for the setup and user guide in docs/user/, material-composer for release notes and any other material). Use whenever you write or edit the README, anything under docs/user/, docs/operations.md, release notes, in-app "Learn more" or help pages, or any help text longer than a sentence. Not for developer or contract docs (architecture.md, build-plan.md, design-tokens.md, metrics.md, runner/).
---

# Lodestar user documentation

**Use the owner's `documentation-skills` to guide the documentation process** (owner, 2026-10-22; `docs/owner-decisions.md` §2.14). This file only adds what is specific to Lodestar.

**Order of authority:**
1. the owner's decisions (§2.14, the audience);
2. this file;
3. `documentation-skills`, which supplies the process, format, voice and gates for everything this file doesn't say.

## 1. Get the skills
**If your session lists them as skills** (`github-docs`, `material-composer`, `document-builder`, `slide-deck`, `training-material-designer`, sometimes with a prefix such as `anthropic-skills:`), invoke them directly.

**Their scripts** (the gates included) are in the installed skill's folder. In a cloud session that's under `~/.claude/skills/synced/`. Find it, or clone the collection when the skill isn't installed:
```bash
GD=$(dirname "$(dirname "$(find ~/.claude/skills -path '*github-docs/scripts/lint_docs.py' 2>/dev/null | head -1)")")
[ -d "$GD/scripts" ] || { git clone --depth 1 https://github.com/dvortsis/skills ../skills; GD=../skills/documentation-skills/skills/github-docs; }
python3 "$GD/scripts/preflight.py"
```
- Run the scripts from the repository root, by their full path (`python3 "$GD/scripts/lint_docs.py" docs/user`), so `docs/user` resolves.
- Don't copy the scripts into this repository.
- The runner attaches `dvortsis/skills` to docs slices and their reviewers.
- The collection was checked on 2026-10-22 at `dvortsis/skills` commit `6556c8b`: preflight READY and self-test 45/45 for github-docs.

| Material | Skill |
|---|---|
| The setup and user guide on GitHub (`docs/user/`) | **`github-docs`** |
| Release notes, quick-start cards, PDFs, slide decks, training and anything else | **`material-composer`**, which picks the renderer |

## 2. Audience
The owner's reader: someone who can install an app from the Unraid app store but may not know how to set it up, how Docker works, Windows' security features and limits, or good security practice.
- Guide them to a safe setup, plainly, matter-of-fact, and without being wordy.
- This is github-docs' "capable installer" (`references/audience.md`). Use it as written.
- Concepts Lodestar adds to the skill's list:
  - **the DHT**;
  - **metadata only:** Lodestar never downloads torrent content;
  - **the content filters**;
  - **torrent details in logs** (§12.1).

## 3. Where Lodestar differs from the skills' defaults
- **The guide lives in `docs/user/`,** because `docs/` holds the developer docs. Pass `docs/user` to `docs_tool.py` and `lint_docs.py`. The top-level `README.md` links `docs/user/README.md`.
- **Facts:**
  - `discover.py --project . --out docs/setup.facts.md`. Commit it, so revisions can compare against it.
  - For behaviour, read the settings registry (`crates/lodestar/src/settings/`) and `docs/architecture.md`.
  - **Never document planned behaviour as built.** Leave it out, or mark it **(planned)**.
- **Brand:** use `brand/brand.json`, which holds the Atkinson Hyperlegible fonts and Riverstone with the §10.18 primary-ink. **Don't regenerate it with `brand.py detect`**, which reads a theme name ("Ayu") as the font.
- **Labels and terms** come from the app itself: each feature's `web/src/lib/features/<feature>/messages/en.json`, plus the shared `web/src/lib/i18n/messages/en.json` and `docs/ui-glossary.md`. The guide's glossary uses the same words.
- **Project facts beat the skills' examples.** For instance, Unraid appdata goes on the cache pool (`/mnt/cache/appdata/lodestar`), not `/mnt/user`, as the templates say.

## 4. Screenshots
github-docs uses only supplied screenshots, and lists the missing ones in `images/SHOTLIST.md` as pending. For Lodestar:
- **Lodestar's own screens** are supplied by the project's capture, slice `S-doc-shots`:
  - Playwright over the fixture data, in Riverstone light;
  - an outline on the element the step refers to;
  - written to `docs/user/images/`.

  Until the capture exists, list them in `SHOTLIST.md`.
- **Other products' screens** (Unraid, Windows, routers) are supplied by the owner. Before committing one, check that it shows no hostnames, public IPs, serial numbers, usernames or tokens, and note the product's version in the caption.
- **Never** show real torrent names, logs written with torrent details on, secrets or setup tokens.

## 5. Diagrams
Use Mermaid, as github-docs allows: one sentence before it and a text equivalent after.
- Lodestar's usual ones:
  - Discover → Fetch → Understand → Index → Serve;
  - the ports through the router and Docker;
  - the VPN path (gluetun);
  - an update and its rollback (§12.2).
- In-app help uses static SVG, following `ui-design-skills`' `infographic-design`.

## 6. Lodestar's safe choices
These come on top of github-docs' safe defaults; each goes where the reader meets it.
- **The web UI (8090) is never forwarded.** For remote access, use a VPN.
- **Forward only the DHT port (UDP 6881)**, and only by choice. Name the trade-off: it gives better reach, and it shows your IP to DHT peers.
- **The VPN template (gluetun):** what it hides (your IP from peers) and what it doesn't.
- **The setup token stays private** until setup is done. The admin password comes from a password manager.
- **API keys and feed tokens:** one per app, and regenerate any that leaks.
- **The content filters:**
  - the child-safety core is always on;
  - changes that weaken it ask for the password (§20.11).
- **Logs:**
  - torrent details stay off;
  - share logs through Download diagnostics, which removes them (§12.1).
- **Updates:**
  - Off until chosen;
  - **Fixes only (recommended)** when turned on;
  - a rollback happens on its own (§12.2).
- **Windows:**
  - the Defender exclusion covers the data folder only;
  - firewall prompts get private networks only.
- **Backups:** where they are, and how to restore one.

## 7. Gates
- From the repository root, run `python3 "$GD/scripts/docs_tool.py" sync docs/user`, then `python3 "$GD/scripts/lint_docs.py" docs/user --strict`, until it reports 0 failed and every warning is fixed or explained.
- Before merge: `python3 "$GD/scripts/docs_tool.py" sync docs/user --check` must report nothing to change.
- Put the `LINT:` line in the PR description.
- Other materials pass `material-composer`'s gate and their renderer's gate.
- Reviewers re-run the gates themselves (`runner/standards.md` §5, item 8).
