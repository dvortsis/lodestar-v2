# R-rc4-cut: the memory and redaction fixes onto release/v0.5.1 for rc.4
**Model:** Opus. A fresh Opus reviewer follows; `/security-review` on the redaction part. Runner brief, 2026-11-05. Owner decision: rc.4 = the memory fixes plus a release-branch Legal Notice (LEGAL-1r, a separate slice); rule packs on by default and the unauthorised-copy pack move to v0.5.2 (owner in chat, 2026-11-05 ~01:15Z; recorded as `docs/owner-decisions.md` §2.40 in the runner's next PR).

## Why
Soak r4 on rc.3 failed the memory gate: `private_bytes` over baseline h12–24 grew at 29.35 MiB/h, 95% CI [28.75, 29.96], against an upper bound ≤ 2.0. Smaps put the growth outside mimalloc's 1 GiB arena, in 128–512 KiB anonymous mappings (about 0.2 per torrent, about 120 kB dirty each), from the static musl build's C allocator. The fixes are on `main`. rc.4 carries them and soak r5 tests them on a frozen image.

## What goes in, exactly (merge commits on `main`; use `git show -m --first-parent <sha>` for each diff)
1. **X-thp-off** (#434, `b926abd592`): `crates/lodestar/src/thp.rs` and its call at start-up, docs row, decisions file.
2. **X-mem-gauges** (#433, `b4fb3f87bf`) and **X-mem-gauges-2** (#442, `bf49050cad`): the memory attribution gauges (`crates/store/src/memstats.rs`, `crates/lodestar/src/memstats.rs`, metrics rows) and the soak-tool changes they need. The soak tools under `docs/soak/tools/**` are used from `main` (the collector and analyzer are pinned by revision, not shipped), so bring only what the shipped binary needs; say which.
3. **X-log-url-redact** (#432, `36a96ff67b`): `crates/log/src/url.rs` only (security fix: URL credentials leaked after a stray `://`; also fixes a char-boundary panic in the IPv6 look-ahead that rc.3 carries).
4. **X-mimalloc-override** (#448, not merged yet): only the `crates/lodestar/Cargo.toml` musl target hunk (`features = ["override"]`) and the `.github/workflows/release.yml` nm check step. The runner messages you its merge SHA; until then build the rest.

Not X-mmap-cache-prune (#406): rc.3 already carries it.

## Do
1. **Branch** `release/rc4-cut` from `origin/release/v0.5.1` (`bd9300dd`, rc.3). Cherry-pick each item with `-m 1`, in the order above.
2. **Resolve conflicts to each fix's behaviour on the release branch's code.** Never pull in a post-cut feature to make one fit. Where a fix calls a helper that only exists on `main`, write the smallest equivalent on the release code or drop the part that exists only because of that feature, and say which. Keep each fix's tests; adapt only what the release code forces, and say why.
3. **Mechanical files:** regenerate `openapi.json`/`schema.d.ts` with the repo's tooling if anything changes them (it shouldn't). `docs/metrics.md`: the release branch's text plus the new rows. `Cargo.lock`: let cargo resolve; no unrelated bumps.
4. **Version:** bump to `0.5.1-rc.4` the way rc.3 did (#407/#408).
5. **Delta report** `docs/releases/v0.5.1-rc4-delta.md`: every changed file and range from rc.3, each tied to a targeted test; what touches the crawler lifecycle or memory behaviour (all of it does: so the soak evidence does **not** carry over, and r5 is a fresh 6 h + 30 min + 24 h run on the rc.4 image); the LEGAL-1r part is listed by the runner when it lands.
6. **Decisions file** `docs/decisions/2026-11-05-R-rc4-cut.md` (short): each conflict, how it was resolved and why it's equivalent.
7. **Checks** on the branch: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; build the musl release target if the toolchain is present (zig), and run the release.yml nm check logic locally once #448's step is in. `/security-review` on the url.rs part.

## Own
The `release/rc4-cut` branch and only the files these PRs touch, plus the delta report and decisions file. **Don't edit** `main` or any slice branch. LEGAL-1r is a separate slice with its own PR into `release/v0.5.1`; don't touch `crates/api/src/auth*`, the setup screen or `docs/legal/**`.

## Done when
Open the PR **against `release/v0.5.1`** (not `main`), titled "R-rc4-cut: memory and redaction fixes onto release/v0.5.1 for rc.4". Add the `full-ci` label. Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the conflicts resolved, and the test results, using the `send_message` tool of the Claude Code Remote MCP server. Never merge; never tag. Push once per round of fixes. No fixed sleeps. Don't background long jobs with `&`.
