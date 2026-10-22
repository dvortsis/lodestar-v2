# DOC-install: User guide, first chapters (Before you start; Install on Unraid and Docker; Keep Lodestar safe)
**Model:** Sonnet (user documentation). **Branch:** `slice/DOC-install`.

**Spec (authority order):**
- `docs/owner-decisions.md` §2.14;
- `.claude/skills/user-docs/SKILL.md`, the project layer. It explains where the owner's `documentation-skills` scripts live in a cloud session. If they're missing, clone `https://github.com/dvortsis/skills` into a scratch dir;
- `github-docs` (the outline, voice and lint gate);
- `runner/standards.md` §4b and review item 8;
- `docs/architecture.md` §11 (deployment) and §12 (security);
- `packaging/unraid/*.xml`, `packaging/docker/README.md`, `docker-compose.yml`, `docs/operations.md`.

## Own
- `docs/user/**` (new).
- The README's "Documentation" pointer section only.

## Build
Scaffold the guide with github-docs' outline, and write only these chapters now:
1. **The home page and task finder.**
2. **Before you start.** What Lodestar is and does, what it needs, and what it never does (no downloads of content; offline by default).
3. **Install Lodestar on Unraid** (Community Applications template) **and with Docker Compose.** Every template field explained, with the recommended answer, PUID/PGID and appdata on the cache pool.
4. **Keep Lodestar safe.** The required chapter, per user-docs §6: never port-forward the web UI, the setup token, passwords, the VPN template, API keys later, backups.

**Facts only from the repo:** run the skill's `discover.py`, and read the templates and code. Windows, Linux and the app screens are **(planned)**: stub those chapters with one line each and nothing invented. Screenshots come later (S-doc-shots). Mermaid diagrams are welcome, each with a text equivalent.

## Common rules
- **Bar and precedence:** `runner/standards.md` is the bar. Read CLAUDE.md, `docs/owner-decisions.md`, and the architecture sections cited.
- **Decisions** go in a new `docs/decisions/<YYYY-MM-DD>-<slice-id>.md` (see `docs/decisions/README.md`), not in §16.
- **Before pushing,** merge `origin/main`; never rebase. Run every check CLAUDE.md lists for the files you touch.
- **Scope:** stay inside **Own**. Shared-file edits needed elsewhere go in the PR description.
- **PRs** stay at or under about 1,500 changed lines, or state the reason in the description.

## Done when
- `lint_docs.py docs/user --strict` reports 0 failed, and `docs_tool.py sync docs/user --check` is clean (commands in user-docs §7). Put the LINT line in the PR description.
- The PR is titled "DOC-install: User guide, first chapters".
