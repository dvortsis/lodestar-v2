# RP-G: the rule-pack gate
**Model:** Opus, with `/security-review`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T23:40Z. Launch once RP-5 (#510) merges.

## Why
RP-0 to RP-6 have built the curated rule packs: SP-1 to SP-4, LU-2, FK-5 and the rest, plus versions, overrides, the Packs API and screen, and RP-4's at-commit entry point. RP-G is the gate that proves them together, as `docs/plan/2026-11-04-v06-rulepacks.md` §"Gates" (RP-G) and architecture §20.8 define it.

## Read first
- `docs/plan/2026-11-04-v06-rulepacks.md`: the RP-G gate text, and Q6–Q16 where they set numbers;
- `docs/architecture.md` §20.6–§20.11;
- `docs/owner-decisions.md` §2.20 and §2.21;
- the decisions files of RP-0 to RP-6, especially `docs/decisions/2026-11-05-RP-5.md`, whose item 7 hands you the SP-4 budget check;
- `crates/filter/tests/props.rs` (the `budget` test as it stands) and `crates/filter/tests/sp4.rs` (`budget_with_the_set_at_its_cap`, measured by hand).

## Do
1. **Corpora:** every pack's corpus and the precision corpus pass through one gate test, `crates/filter/tests/packs_gate.rs`, which fails on any regression.
2. **Budget** (`props.rs`, which CI runs): with every pack on, **SP-4's set loaded at its 1M cap** and RP-4's at-commit entry point included, the core, 1%, 10% and mixed cases stay under 50 µs scaled. They also stay under it with a pending pack version in shadow. Add this to the CI job that already runs `props.rs`; don't create a new job unless one is needed.
3. **Upgrade without release:** when the applied version is older than the one built, it stays pending until accepted, then releases nothing, and it pauses overrides that weaken.
4. **Guardrails:** every new op that weakens is refused without the password, through the API.
5. **E2E:** a Playwright run through the Packs screen (`web/e2e/packs.spec.ts`): view, accept a pending version, set an override, and the password prompt on a weakening change. Use Chromium at `/opt/pw-browsers` (`web/e2e/README.md`).
6. **Decisions file:** `docs/decisions/2026-11-06-RP-G.md`, with the measured numbers.
7. **If the gate fails anywhere,** don't loosen it. Report to the runner with numbers.

## Own
- `crates/filter/tests/packs_gate.rs`, `crates/filter/tests/props.rs`, `web/e2e/packs.spec.ts`;
- corpus fixtures under `crates/filter/tests/corpus/**`, if you need new ones;
- the CI line that runs `props.rs` (minimal);
- the decisions file;
- your own `runner/slices.yaml` gate entry.

Don't edit `crates/filter/src/spam_sigs.rs`: X-sp4-followups may be running there. If the gate needs a test hook in the source, message the runner.

## Done when
These pass:
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace` (per crate if the disk is short; say so);
- `npm run check` and `npx playwright test e2e/packs.spec.ts` in `web/`;
- `cd tools/status-connector && npm test`.

Branch `slice/RP-G`. Open the PR titled "RP-G: Rule-pack gate". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps: wait on conditions with deadlines of at least 10 s. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the gate numbers, using the `send_message` tool of the Claude Code Remote MCP server.
