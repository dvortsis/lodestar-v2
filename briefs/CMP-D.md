# CMP-D (Sonnet, docs): user guide on companions, per platform

Assigned by the runner on 2026-11-11; relaunched in a fresh session on 2026-11-12 (the first assignee declined a peer-message assignment, correctly). The slices.yaml entry is CMP-D, spec architecture §18.10 (docs/architecture.md:3218 onwards) and docs/owner-decisions.md §2.14. Use the user-docs skill, which builds on github-docs.

## 1. Read first
- **§18.10 Companion manager:** the catalogue, platform modes, detection, auto-configuration, guided setup and safety rules.
- **The code that ships the behaviour you document, on current main:**
  - `crates/companions/src/` (catalogue.rs, compose.rs, unraid.rs, detect/, configure/, guided.rs, manager.rs);
  - `crates/lodestar/src/cli/` (the `companion` subcommand);
  - `web/src/lib/features/companions/**` and the wizard's download-client step (CMP-9c);
  - the Sources chip (CMP-9b).
- **`docs/operations.md`,** which you just wrote. Link to it for CLI detail; don't duplicate it.

## 2. Do
Write `docs/user/companions.md` (owned). If the guide's chapter numbering needs a numbered file, use `07-...` and record that in the decisions file.

1. **What a companion is.** qBittorrent, Prowlarr and the others in the catalogue, each with what it's for. Recommend the safe default: qBittorrent as the download client.
2. **Per platform, as task procedures with results:**
   - **Docker and Compose:** generated compose services from `lodestar companion compose`.
   - **Unraid:** pre-filled CA templates.
   - **Windows and bare-metal Linux:** managed child processes, install and remove.
   - Real commands and real UI labels only. Mark anything newer than v0.5.1 with "(newer than v0.5.1)", as operations.md does.
3. **The safety rules from §18.10 are the replacement check, so all must hold in the text:**
   - no Docker socket; managed-container mode through the restricted socket proxy is advanced and off by default, so say why;
   - companion ports stay on the internal network;
   - `network_mode: service:gluetun` where a VPN is used;
   - a companion's WebUI is never exposed beyond the LAN, and the router never forwards it.
4. **Detection and auto-configuration:** what Lodestar detects, what it configures for you, and what you still set yourself. Include the hand-over and guided entries as the UI shows them.
5. **Troubleshooting inside the chapter:** a companion not detected, credentials refused, a port in use.
6. **Screenshots** from CMP-9a's e2e, if they exist under web/e2e. Otherwise use SVG illustrations under `docs/user/images/companions/**` with a SHOTLIST entry.
7. **`docs/user/README.md` index:** a shared edit adding one line for the chapter.

## 3. Don't
- PK-guide-2 has merged (#704), so you may add your entries to glossary.md, index.md, revisions.md and appendix-a-answers.md: small, additive edits only.
- Don't edit docs/operations.md, chapter 04 or 06, `runner/`, packaging or code.

## Owns
- `docs/user/companions.md` (or its numbered name), and `docs/user/images/companions/**`.
- A shared one-line edit to `docs/user/README.md`.
- A decisions file, `docs/decisions/2026-11-11-CMP-D.md`.

## Bar
- Every fact is checked against the code on main, with file:line in your notes.
- `docs_tool.py sync` and `sync --check`, and `lint_docs.py --strict`, with 0 failed and 0 warnings.
- No dangling anchors. No 12-digit numbers and no personal data.
- No /security-review: the runner gives a docs-only exemption under standards §7.
- Branch `slice/CMP-D`, PR titled `CMP-D: <title>`. Follow the steward skill, including a fresh self-review.
- Expect about 400–600 lines. Reply "ready" with the PR number and head SHA when CI is green.
