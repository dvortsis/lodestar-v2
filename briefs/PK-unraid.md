# PK-unraid (Sonnet, helper): Unraid templates without the unused hot and cold path fields

Assigned by the runner on 2026-11-11. Deps met (`ext:v051-soak-verdict` holds). About 150 lines.

## Read first
- The `PK-unraid` entry in `runner/slices.yaml`.
- `docs/plan/2026-11-06-m6-gap-audit.md`:
  - §2.2 row "Split paths" (~line 42);
  - ~line 68-69;
  - Q3 and Q4 (~lines 212-213);
  - §4.3 "PK-unraid" (~line 198), which is the spec.
- `packaging/unraid/*.xml` and `packaging/unraid/validate.py`; `docs/user/02-install-lodestar-on-unraid-and-docker.md` (its "Hot path" subsection); `docs/user/images/unraid-*.svg` and how they're generated (look for the generator script and `docs/user/images/SHOTLIST.md`).
- The project skill `.claude/skills/user-docs/SKILL.md` (and the owner's `github-docs` it layers on) for the chapter 2 edit.

## Owns
- `packaging/unraid/**`, `docs/user/02-install-lodestar-on-unraid-and-docker.md`, `docs/user/images/unraid-*.svg`.
- One `docs/user/images/SHOTLIST.md` entry (a shared edit, approved).
- `docs/decisions/2026-11-11-PK-unraid.md`.

## Build
1. Remove the hot and cold path fields from `lodestar.xml` and `lodestar-gluetun.xml` (Q3). Without the mapping, hot-column snapshots stay in Data's `hot/`, and hot columns rebuild from SQLite anyway, so removing them is safe.
2. Make `validate.py` refuse a field whose description says "not used yet". The Peers (TCP) field's "(planned)" stays, because the port is mapped for the peer listener. Add a validator test, or extend the existing one, and show that it fails on a template that still has the field.
3. Leave the worker template's labelling as it is (Q4).
4. Regenerate the form illustrations from the templates with the repo's generator, never by hand.
5. Fix chapter 2's "Hot path" subsection. Remove it, or say plainly that the Data folder holds everything, for someone installing from the Unraid app store. Follow user-docs: plain, brief, the safe choice stated. The lint gate passes.
6. In the decisions file, say what users who already mapped `/data/hot` or `/data/cold` should do. Probably nothing: say so, with the reason, after checking `services.rs:~143` and `config.rs`.

## Checks
`python3 packaging/unraid/validate.py` (and its tests), the user-docs/github-docs lint gate, and any CI job that checks the templates or images. No secrets, no 12-digit numbers.

## Process
- Follow `.claude/skills/steward/SKILL.md`. Branch `slice/PK-unraid` from the latest `origin/main`; PR title `PK-unraid: <title>`.
- When CI is green, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg): `PK-unraid: PR #<n> ready for review, head <sha>`. If blocked: `PK-unraid: blocked on <what>`. The runner reviews and merges; you never merge.
