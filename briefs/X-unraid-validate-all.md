# Brief: X-unraid-validate-all (Sonnet, helper, cloud-testing slot)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-unraid-validate-all` from `origin/main`. **PR title:** `X-unraid-validate-all: Unraid validator checks every template's Requires and Overview; CI runs it`.

**Why now:** this is testing work, so it runs in a cloud-testing slot (owner §2.61/§2.62, 2026-11-12). The Unraid templates are what the Unraid test lane and real users install. Today nothing in CI runs `packaging/unraid/validate.py`, so a broken template ships unseen. It was split from PK-unraid (#680), whose title promised this.

## Read first
- `packaging/unraid/validate.py` (97 lines), `packaging/unraid/test_validate.py`, and the three templates `lodestar.xml`, `lodestar-gluetun.xml` and `lodestar-worker.xml`.
- `.github/workflows/ci.yml`: the `changes` job (its outputs), the `soak-tools` job as the pattern for a small Python job, and `ci-ok`'s `needs` list.
- `.github/scripts/ci_changes.py` and `.github/scripts/test_ci_changes.py`: how each path filter becomes a `changes` output.
- architecture §11 (packaging) and `docs/user/02-install-lodestar-on-unraid-and-docker.md` for what each template is for.

## What to build
1. **The validator checks every template in the folder,** not only the three in `TEMPLATES`. A `*.xml` file in `packaging/unraid/` with no rules entry fails, with a message naming it, so a new template can't skip the checks.
2. **Overview:** non-empty on every template (already required), and its first sentence says what the container is. A template whose app needs a newer Lodestar says so in the Overview (the worker's "Requires Lodestar v0.6+" stays checked).
3. **Requires:**
   - Every template has a `<Requires>` element.
   - Where a template depends on something else, it must be non-empty and name the dependency:
     - **`lodestar-worker.xml`:** names Lodestar v0.6 or newer as the main instance (already true; keep it checked);
     - **`lodestar-gluetun.xml`:** its `ExtraParams` carry `--network=container:gluetun`, so it can't start without a running container named `gluetun`, yet its `<Requires>` is empty today. **Fix the template** (the runner approves this one outside path): `<Requires>` names a running Gluetun container called `gluetun`. Keep it plain and short for an Unraid user, in line with `docs/user/02-…` and the user-docs skill's tone, and check the wording against what that guide already says. The validator then checks that any template with `--network=container:<name>` names `<name>` in `<Requires>`;
     - **`lodestar.xml`:** no dependency, so `<Requires>` may stay empty.
4. **Tests** in `test_validate.py`, standard library `unittest` only:
   - each new rule fails on a broken copy of a template (written to a temp dir) and passes on the real ones;
   - an unlisted `*.xml` fails;
   - the real folder passes as a whole.
5. **CI:**
   - Add an `unraid` output to `ci_changes.py`: true for changes under `packaging/unraid/**`, for the validator's own files, and on `main`, schedule and dispatch runs, like the other outputs. Add its cases to `test_ci_changes.py`.
   - Add one job `unraid-templates` to `ci.yml`, modelled on `soak-tools`: `needs: changes`, `if: needs.changes.outputs.unraid == 'true'`, ubuntu, setup-python 3.12, then `python packaging/unraid/validate.py` and `python -m unittest discover -s packaging/unraid -p 'test_*.py' -v`.
   - Add it to `ci-ok`'s `needs`.
   - No new permissions, secrets, actions or third-party packages. Pin nothing new.

## Owned paths
`packaging/unraid/validate.py`, `packaging/unraid/test_validate.py`, `.github/workflows/ci.yml` (the `changes` outputs line, the new job and `ci-ok`'s needs only), `.github/scripts/ci_changes.py` and `.github/scripts/test_ci_changes.py` (the new output only), `packaging/unraid/lodestar-gluetun.xml` (the `<Requires>` text only), and `docs/decisions/2026-11-12-X-unraid-validate-all.md`.

Ask the runner before any other file. If the gluetun `<Requires>` change makes the guide's illustration of the form (`docs/user/images/unraid-*.svg`) visibly wrong, say so in the PR. Don't regenerate it in this slice.

## Checks before each push
- `python3 packaging/unraid/validate.py`
- `python3 -m unittest discover -s packaging/unraid -p 'test_*.py' -v`
- `python3 -m unittest .github/scripts/test_ci_changes.py` (or however that file's header says to run it)
- `actionlint` if it's installed. If not, check the YAML with `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))"`.

## Done
Follow `.claude/skills/steward/SKILL.md`, push once per round, and when ci-ok is green, message the runner "ready <pr> <full 40-char sha>".
