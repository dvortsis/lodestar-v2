# Codex light review of Lodestar v0.5: lower-priority areas

These instructions are for a cheaper, faster model. The high-risk areas (parsers, auth, filtering) are in `codex-review-v0.5.md`; run those on the stronger model.

Run **one session per area**, in any order, and skip any you don't care about. In each session, paste **Common instructions** first, then the block for that area.

Work on a fresh clone of `https://github.com/dvortsis/lodestar-v2` at `main`.

---

## Common instructions (paste first, every session)

You are doing a **light, read-only bug review** of one area of **Lodestar**, a self-hosted torrent-metadata search engine: one Rust binary with an embedded Svelte 5 web UI, running on Docker, Unraid or Windows. Don't edit files, commit, push or comment on GitHub.

**Read first:**
- `docs/owner-decisions.md`, which holds the rules;
- `runner/standards.md` §2 and §4 (code and UI rules) and §5 (the review checklist).

**Report only things that are clearly wrong and that a user or operator would hit.** That means:
- a broken flow or a wrong value shown;
- a feature that doesn't work, or a dead link or route;
- a setting that does nothing;
- an install step that fails;
- a CI job that can pass while what it checks is broken;
- docs that contradict the code;
- an accessibility failure, such as no keyboard access, a missing label or no visible focus.

**Don't report:**
- style, naming, formatting or wording preferences;
- refactor ideas, or "could be cleaner";
- speculative performance issues;
- anything you can't point to in the code.

Be honest about uncertainty. If you're not sure, use `Confidence: low`, or leave it out.

**Output:** at most **8 findings**, most useful first, in this exact format:

```
### N. <one-line title>
Location: <path>:<line>
What happens: <concrete steps or input, and the wrong result>
Expected: <what should happen, citing the doc / decision / comment>
Confidence: high | medium | low
```

End with `Checked and clean: <files you read>`. If nothing is clearly wrong, say so.

---

## Area A: web UI behaviour

**Scope:**
- `web/src/routes/`
- `web/src/lib/features/`: `search`, `detail`, `discover`, `compare`, `alerts`, `shell`, `shortcuts`, `auth`

**Spec:** `docs/ui-glossary.md`, `docs/metrics.md`, `docs/design-tokens.md`, architecture §10.

**Check:**
- **The URL:** state that doesn't round-trip through it. Reload or Back gives a different view.
- **States:** loading, empty and error states that are missing or show the wrong text.
- **Numbers:** values formatted against `docs/metrics.md`, such as sizes, dates, counts or "≈".
- **Keyboard:**
  - shortcuts that fire while you type in a text field;
  - focus that's lost after a dialog closes;
  - controls that can't be reached with Tab.
- **Labels:** words that don't match the glossary.
- **Hard-coded colours:** hex values used in components (banned; tokens only).
- **The "Show adult content" setting:** when it's off, adult items must not appear anywhere in the UI.

---

## Area B: packaging and install

**Scope:**
- `packaging/docker/`, `packaging/unraid/`, `packaging/systemd/`, `packaging/linux/`, `packaging/windows/`
- `docker-compose.yml`
- `docs/operations.md`, `docs/setup.facts.md`, `docs/user/`

**Spec:** architecture §11 and §19.6, `docs/owner-decisions.md` §2.14 and §2.23 (Windows builds ship unsigned for now).

**Check:**
- **Ports, paths and environment variables** that differ between the Unraid template, the compose file, the systemd unit, the Dockerfile and the docs.
- **User and group IDs:** PUID/PGID or permission steps that would fail on a fresh install.
- **Stopping and the health check:**
  - a stop timeout shorter than the app's 10-second shutdown;
  - a health check that points at the wrong path.
- **Windows:** installer or service steps that don't match `packaging/windows/`, and a SmartScreen explanation that's missing or wrong.
- **Install guide:** steps in `docs/user/` that a first-time Unraid user couldn't follow as written.

---

## Area C: CI and release workflows

**Scope:**
- `.github/workflows/ci.yml`, `web.yml`, `e2e.yml`, `release.yml`
- `.github/scripts/`

**Spec:** the comments at the top of each workflow; `docs/owner-decisions.md` §2.18 and §2.22 (`ci-ok` is the required check).

**Check:**
- **`ci-ok`:** it reports success even though a job in its `needs` failed.
- **Path filtering:** a change to real code that `.github/scripts/ci_changes.py` would wrongly skip.
- **Pinning:** actions that aren't pinned to a full commit SHA.
- **Release:** steps that would publish without the signed manifest or checksums.
- **Secrets:** a secret exposed to `pull_request` runs from forks.

`e2e.yml`'s `commit-baselines` job was already security-reviewed, so only report something clearly broken there.

---

## Area D: non-auth API endpoints

**Scope:**
- `crates/api/src/`: `search.rs`, `facets.rs`, `suggest.rs`, `torrents.rs`, `discover.rs`, `compare.rs`, `saved.rs`, `collections.rs`, `user_items.rs`, `export.rs`, `jobs.rs`, `insights.rs`, `ops.rs`, `error.rs`, `extract.rs`

**Spec:** architecture §9, `docs/metrics.md`, `docs/query-language.md`.

**Check:**
- **Pagination:** cursors that skip or repeat results.
- **Errors:** responses that don't use the error envelope in `error.rs`, or use the wrong HTTP status.
- **Parameters:** query parameters that are accepted but ignored, or have no upper bound (`limit`, page size, export size).
- **Response fields:** fields documented in `docs/metrics.md` that are missing, or come back as `null` when they shouldn't.
- **Hidden torrents:** a torrent hidden by filters, or by the adult setting, still returned by some endpoint, such as export, compare, discover, suggest or collections.

---

## Area E: docs consistency

**Scope:**
- `README.md`, `docs/user/`, `docs/operations.md`, `docs/query-language.md`

Compare each against the code it describes.

**Check:**
- **Query syntax:** examples in `query-language.md` that the parser in `crates/search` would reject or interpret differently.
- **Settings:** settings, defaults or command-line flags in the docs that don't exist in the code, or have a different default.
- **Links:** links to files or sections that don't exist.
- **The user guide:** steps that recommend an unsafe choice, against `.claude/skills/user-docs/SKILL.md`.
