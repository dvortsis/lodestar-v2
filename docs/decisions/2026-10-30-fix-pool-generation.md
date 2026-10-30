# fix/pool-generation decisions (PR pending)

- **Entry generations are pool-wide, so a stale timer can't expire a replacement entry's lookup (runner, 2026-10-30, from the CR-ADM design review).**
  - *The defect.* Each pool entry's `generation` started at 0 and was bumped per entry. A key evicted with a timer still queued, then found again, came back as a new entry counting from 0. The replacement reached the same generation as the old timer, and when that timer fired early it expired the new lookup. The lookup's receivers dropped, so the actor cancelled work that was still useful, and the key backed off.
  - *The fix.* Every bump now takes the next value of one `u64` counter in the pool, which never wraps in practice and never repeats in a pool's life. Timers and owned requests compare it exactly as before.
  - *Why its own PR.* It's fixed unconditionally, apart from CR-ADM, so the candidate's A/B isn't confounded with a bug fix. Test: `a_stale_timer_cannot_expire_a_rediscovered_keys_lookup` (`crates/sources/tests/pool.rs`), which fails on `main`.
  - *The goldens.* The three CR-SIM goldens are unchanged, byte for byte: the collision doesn't change any reported counter in those runs.
  - *Size.* `Entry` grows by 8 bytes, from 72 to 80 on x86_64: 1.6 MB at the default `max_pending` of 200,000. That uses all the slack `diagnostic_entry_storage_is_bounded` allows, so the next field that crosses an alignment boundary needs that budget re-decided.
    - A wrapping `u32` would keep 72 bytes, and it couldn't realistically collide (2^32 bumps inside the longest timer, about 32 min). It was rejected so that no wraparound argument is needed.
