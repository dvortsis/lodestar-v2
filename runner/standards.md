# Build standards

These are the single bar for every slice session and every review. Briefs add slice-specific checks on top; they never lower these. The sources of truth, in order:
1. `CLAUDE.md`
2. `docs/architecture.md`
3. the contract docs: `docs/design-tokens.md`, `docs/metrics.md`, `docs/ui-glossary.md`
4. `ui-design-skills`, in its own precedence order with the owner's choices on top (§3), applied by spirit for Svelte
5. the slice brief

## 1. Checks before every push
**Rust (any crate touched):**
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- any bench gate the brief names (`size-report --gate`, `search-bench --gate`)

**Web (anything under `web/` touched):**
- `npm ci`
- `npm run check`: svelte-check, eslint, prettier, the hex and arbitrary-value bans, messages
- `npx vitest run`
- `npm run build`
- `npx size-limit`
- `npx playwright test` for any route or component with an e2e spec. Chromium is at `/opt/pw-browsers`; never run `playwright install`.

**Always:** `git fetch origin main` and merge it (never rebase a shared branch) before opening or updating a PR. Never push red.

## 2. Code rules
- **Delegation (CLAUDE.md):** untrusted-input parsers, on-disk formats, concurrency-heavy code and security code are Opus slices only. A helper that finds itself needing one of these stops and says so in the PR.
- **Ownership:** edit only the paths the brief owns. Shared files (router composition, migrations, `web/package.json` beyond the one script line a brief allows, layout, message merge output) are changed by the runner or by the slice that owns them.
- **Dependencies:** pinned exactly. A new dependency needs the brief's explicit approval.
- **Tests:**
  - every module ships unit tests;
  - parsers also get property or fuzz tests;
  - UI primitives get keyboard and state tests;
  - formatters are tested byte-exact against `docs/metrics.md`.
- **Decisions:** any choice that changes or clarifies the architecture gets a short entry at the end of `docs/architecture.md` §16, in the same PR.
- **Commits and PRs:** an imperative subject of 72 characters or fewer, and a body that says why. No model identifiers anywhere.

## 3. UI design skills (`dvortsis/skills`, `ui-design-skills/skills/<name>/SKILL.md`)
**The skills are the authority for UI decisions** (CLAUDE.md, §10.0). Every UI slice reads the skills listed in its brief before writing code, and every UI review checks against them. The repo is public: `git clone --depth 1 https://github.com/dvortsis/skills`.

- **Precedence** (the skills' own order, with the owner's choices on top):
  1. the owner's explicit choices: Svelte 5, smallest and fastest first, `minimalist-visual-style`, Atkinson Hyperlegible Next/Mono, Riverstone with plum `#9381B6`, 24 theme families × light/dark, brand assets per §10.19;
  2. accessibility and functional correctness;
  3. Lodestar's existing product language where no skill owns the decision (the composition bar, dense expandable rows);
  4. skill defaults;
  5. `minimalist-visual-style`.
- **One owner per decision.** The §10.0 table says which skill owns which subject. For a question about spacing, read `layout-spacing`, not a screen skill.
- **By spirit, for Svelte:**
  - Where `data-visualization-design` assumes Tremor/React, keep its data contracts, states, accessible alternatives, performance rules and validation hooks. Implement them with the in-house SVG primitives and lazy uPlot.
  - Where `infographic-design` assumes AntV, use static SVG.
  - `application-interface-design`'s Tailwind + daisyUI guidance applies literally.
- **Which skills a slice reads:**

| Slice kind | Skills |
|---|---|
| Any screen or primitive | `layout-spacing`, `layout-grids`, `responsive-layout`, `typography-ui`, `component-geometry`, `iconography-ui`, `motion-ui`, `application-interface-design`, `minimalist-visual-style` |
| Charts, formatters | + `analytics-chart-selection`, `data-visualization-design` |
| Insights and any dashboard page | + `dashboard-storytelling`, `analytics-validation` |
| Facets, counts, KPIs | + `analytics-validation` |
| Onboarding explainer (wizard "How it works") | + `infographic-design` |
| Themes, colour | colour is owner-defined (§10.18); contrast rules from `design-review` and `application-interface-design` |
| Milestone and screen sign-off | `design-review`: blocking / important / polish |

## 4. UI rules (from §10 and the contract docs)
- **Tokens only.** No hex values and no arbitrary Tailwind values (`[#…]`, `-[13px]`).
- **Spacing** uses Tailwind's **default scale** (`p-4` = 16 px), restricted to the steps that match the §10.2 ramp: 0, 0.5, 1, 1.5, 2, 2.5, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14. This was decided 2026-10-22 and is recorded in §16. The ramp itself is `layout-spacing`'s; only the utility mapping was decided.
- **Radii, control heights, type scale and motion** come from `docs/design-tokens.md` only.
- **Numbers** go through `src/lib/format` only.
  - The client never computes counts, cohorts or percentages.
  - Null is "—", never 0. Estimated and sampled values show "≈".
  - Every number on screen has a `data-testid`.
- **Status** is always icon plus text, never colour alone. Focus is always visible, and targets are at least 44 × 44 px under `pointer: coarse`.
- **Strings** live in the feature's `messages/{en,zh}.json`, worded per `docs/ui-glossary.md` and §10.14.
- **Budgets (§10.1):** search-route JS ≤ 45 KB, CSS ≤ 20 KB, total first load ≤ 100 KB gzipped. Detail drawer ≤ 15 KB, Insights ≤ 45 KB.

## 5. Review checklist (every PR, by a fresh Opus reviewer)
1. **Scope:** only owned paths changed, and nothing is missing from the brief's "Do" list.
2. **Correctness against the spec sections the brief cites.** Any contradiction is blocking.
3. **Checks:** the reviewer re-runs §1 itself and doesn't trust the PR text.
4. **Tests:** they cover the behaviour, including edge cases and failure paths, not only the happy path.
5. **Standards:** §2 and §4 above.
6. **UI slices: the design skills (§3).** Check each subject against its owning skill: spacing against `layout-spacing`, states and forms against `application-interface-design`, charts against `analytics-chart-selection`, and so on. Run a `design-review` pass on each finished screen. A skill violation is blocking unless one of the owner's explicit choices overrides it.
7. **Security:** for anything touching auth, credentials, untrusted input, filters, the cluster or companions, run `/security-review` as well.

**Severity:**
- **Blocking:** a spec violation, a broken or failing check, a wrong value, a missing test for new behaviour, a file outside ownership, an unapproved dependency, a security issue.
- **Important:** a defect or gap that later slices would inherit. These get fixed in the same PR when cheap; otherwise they go into the next brief that touches the area.
- **Polish:** optional.

A PR merges only when CI is green and there are no blocking findings.
