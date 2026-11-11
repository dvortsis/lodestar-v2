# PK-ops: docs/operations.md, what exists today, install sections, the CLI, and "I can't reach the UI"
**Model:** Sonnet, docs. A fresh Opus reviewer follows. Runner brief, 2026-11-11T16:30Z. Epic M6 (ws-m6); it goes straight into G-m6. The dep PK-cli merged as #696 (647e32e1).

## Read first
- `CLAUDE.md` (User documentation), `runner/standards.md`, and the project skill `.claude/skills/user-docs/SKILL.md`. Use the owner's documentation skills, as the CLAUDE.md "User documentation" section says.
- `docs/plan/2026-11-06-m6-gap-audit.md`: the operations.md row (~100), the PK-ops row (~122), and the **PK-ops paragraph (~199)**, which is the scope.
- `docs/architecture.md` §10.10.6 (it says operations.md has an "I can't reach the UI" section), §11 and §12.
- `docs/decisions/2026-11-11-PK-cli.md` (#696): what `lodestar settings get / set / reset` and `reset --all` do, what they refuse, and the exit codes.
- The current `docs/operations.md`, in full.

## Do
1. **§1:** replace the "Today/Planned" table with what's built today. The Docker image, tarballs, the MSI and the portable zip are all built now, and so are the CLI subcommands.
   - Check every claim against the code and `lodestar --help` (build it: `cargo run -q -p lodestar -- --help`, and `--help` on each subcommand).
   - Keep anything still unbuilt marked as planned, and say what it is.
2. **§2:** make the Docker and Windows sections real.
   - Drop "(planned)".
   - Use the facts in `docs/setup.facts.md` and `packaging/**`: the compose file, the Unraid template, the MSI options, the service, and the portable zip.
   - Link to the user guide (`docs/user/`) rather than duplicating it.
3. **A CLI section:** every subcommand from `--help`, with one line each and its safe use. It covers:
   - `settings get / set / reset / reset --all`, with what `set` refuses (security, privacy and confirmation-tagged fields, and secrets) and why;
   - `healthcheck`;
   - the update helper, only as far as a user would run it.
4. **"I can't reach the UI":**
   - `LODESTAR_HTTP_BIND`;
   - `lodestar settings reset network.http_bind`;
   - `reset --all`;
   - `healthcheck`.
   - Say plainly that if an http_bind set from the CLI can't be bound, `serve` fails at start: there's no revert like the UI's. `lodestar settings reset network.http_bind` is the way out (from #696's review).
   - Each case gets a short, numbered procedure with its result.
5. **Hot/cold leftovers** (runner follow-up after #680):
   - `docs/operations.md:~182-188` (the "Split paths (optional)" bullet and its "(Split storage is planned…)") and `:~206` ("`cold.db` and the `data/hot`/`data/cold` split are **planned**").
   - Make them match what's built. Check `crates/lodestar` for the data folder layout; if the split isn't built, say once, plainly, that everything lives under `/data`, and remove the advice about the two mappings.
6. **Times are UTC.** Define terms, recommend the safe choice, keep it plain and brief, and use no model identifiers.

## Owns
- `docs/operations.md` only. Ask the runner first before touching anything else. For example, if the user guide needs a matching line, name it and the runner routes it to PK-guide-2 or PK-guide-3.

## Done when
- The user-docs skill's gates pass, and links resolve.
- Every command and flag in the file exists in `--help` at your head.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge.
  - Branch `slice/PK-ops`. PR title `PK-ops: docs/operations.md: what exists today, install sections, the CLI, and I can't reach the UI`.
  - Estimated ~500 lines.
  - Once CI is green, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready" with the PR number and full head SHA, using the Claude Code Remote MCP `send_message`.
