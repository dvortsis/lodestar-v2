# X-wk-busy-retry (Opus core), then X-wk-pause-text (in the same session)

Session: session_01MMjhnAgARNjXHd2povCJ2f (slot 4), which already knows `leases.rs` and the Busy/Unknown semantics from #628. Follow `.claude/skills/steward/SKILL.md`.

## 1. X-wk-busy-retry: branch `slice/X-wk-busy-retry`, PR `X-wk-busy-retry: <title>`
**Owns:** `crates/lodestar/src/worker/runtime.rs`, plus a test file next to it, and a decisions file `docs/decisions/2026-11-08-X-wk-busy-retry.md` if anything is decided.

**Problem (#628 review):** the worker's claim loop drops every denied key. When the coordinator answers `DenyReason::Busy`, the key is lost. That happens when the writer's queue is full (§19.5), when the store check died or came back short for the batch (#607), or when one key's read failed (#628).

**Change:**
- Retry Busy denials only, with bounded exponential backoff and jitter. Choose a cap and an attempt limit, and say why in the PR.
- Keep the retried keys bounded in memory, so a coordinator stuck on Busy can't grow the worker's state without limit.
- Unknown, and every other denial, still drops the key, exactly as today.
- Update `server.rs`'s `KeyCheck` doc line "(Today's worker drops any denied key; a busy retry is a follow-up.)" only if that file is trivially in reach. Otherwise name it in the PR for the runner.

**Tests:**
- Busy is retried and then claimed.
- Busy keeps failing until the limit, then the key drops with a log line.
- Unknown drops immediately.
- The retry memory stays bounded.
- No fixed sleeps: use tokio's paused time only if the crate already allows it (never add tokio `test-util` to the `lodestar` package), otherwise inject a clock or condition waits with deadlines of at least 10 s.

## 2. X-wk-pause-text (Sonnet-class text), after #1 is open: branch `slice/X-wk-pause-text`, PR `X-wk-pause-text: <title>`
**Owns:** `web/src/lib/features/settings/logging/messages/{en,zh}.json`.

**Problem:** the `settings_logging_ev_workers_pause_not_saved_*` texts say the pause can be redone in Workers. For a removed (unknown) worker (#628's `code=unknown_worker`) that's wrong. Make the help cover both cases, plainly, in en and zh.

**Scope:** a workers-screen help string outside these files goes in the PR description first.

**Checks:** `npm run check`, `npx vitest run` and `npm run build` in `web/`.
