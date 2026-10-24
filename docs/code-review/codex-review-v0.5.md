# Codex review of Lodestar v0.5: agent instructions

Run one session per area, in order. Stop when your usage runs low; area 1 alone is worth doing.
Each session: paste **Common instructions** first, then that area's block.
Work on a fresh clone of `https://github.com/dvortsis/lodestar-v2` at `main` (v0.5 passed its gate at commit `4667277`).

---

## Common instructions (paste first, every session)

You are reviewing **Lodestar**, a Rust DHT crawler and torrent-metadata search engine, for **real bugs only**. This is a read-only review: don't edit files, commit, push, open PRs or comment on GitHub.

Read these first, briefly:
- `docs/owner-decisions.md`: the owner's decisions. These are the rules.
- `runner/standards.md` §2 (code rules) and §5 (review checklist).
- The sections of `docs/architecture.md` that your area names.

**What counts as a finding:**
- **Crashes:** a panic, an unbounded allocation or stack depth, an infinite loop, or a hang.
- **Wrong results:** a wrong parse result, a value accepted that the spec rejects, or one rejected that it allows.
- **Security holes:** an auth bypass, a timing leak, a missing limit, injection, or a path that escapes its folder.
- **Concurrency bugs:** races, deadlocks, or lost updates.
- **Breaking an owner decision**, or a guarantee the code's own docs or comments make.

**What doesn't count:** style, naming, formatting, missing docs, refactor ideas, performance without a concrete limit being broken, or anything a test in the same crate already proves correct.

**Before reporting anything:**
- Trace each candidate from a real entry point, such as bytes off the network or an HTTP request, to the failure.
- Check the existing tests (`tests/`, `#[cfg(test)]`, `fuzz/`) for that case.
- If a test already covers it, or you can't build a realistic input, drop it.
- Write a failing test only if you can run `cargo test -p <crate>` locally. Never fake output.

**Output:** at most **10 findings**, most severe first, in this exact format:

```
### N. <one-line title>
Severity: critical | high | medium | low
Location: <path>:<line>
Input / scenario: <concrete bytes, request, or interleaving>
Expected: <what should happen, citing the spec / owner decision / doc comment>
Actual: <what the code does, and why — quote the lines>
Confidence: high | medium | low
Suggested fix: <one or two sentences>
```

End with a line `Checked and clean: <files you read fully that had nothing>`. If you find nothing real, say so. That's a useful result.

---

## Area 1: untrusted-input parsers (highest priority)

**Scope:**
- **`crates/bencode/src`:** `decode.rs`, `value.rs` and `encode.rs`.
- **`crates/metainfo/src`:** `info.rs`, `torrent.rs`, `hash.rs` and `text.rs`.
- **`crates/dht/src`:** `krpc.rs`, `compact.rs`, `bep33.rs` and `secure_id.rs`.

**Spec:** architecture §5 (the crawler and KRPC) and §6 (metadata fetch), BEP 3, 5, 9, 33, 42 and 51. Also the hostile-input limits in `runner/standards.md` §2.

**Look hardest at:**
- **Bencode.** Integer edge cases: leading zeros, `-0`, overflow, an empty `i e`. String length prefixes larger than the remaining input. Nesting depth. Dictionary keys out of order or duplicated. Trailing bytes. Re-encoding that isn't byte-identical when it must be (that breaks the info-hash).
- **Metainfo.** Info-hash computed over anything except the exact raw `info` bytes. v1, v2 and hybrid handling. Piece length and piece count mismatches. File paths with `..`, absolute paths, empty components, or names that aren't valid UTF-8. Total-size overflow. Huge file lists.
- **KRPC and compact.** Transaction-ID handling. `nodes` and `values` lengths that aren't a multiple of 26, 38 or 6. IPv6 versus IPv4 confusion. Ports set to 0. Error replies. Bounds on BEP 33 bloom filters.
- **Every limit:** is it enforced **before** the allocation or the recursion it guards?

---

## Area 2: authentication, API keys and request limits

**Scope:**
- **`crates/api/src/auth.rs` and `crates/api/src/auth/`:** `guard.rs`, `http.rs`, `limit.rs` and `proxy.rs`.
- **Key handling:** `crates/api/src/keys.rs`, if present, and `dto/keys.rs`.
- **Wiring:** how the routes in `crates/api/src/` apply the guard.

**Spec:** architecture §12 and §20.11. Also `docs/owner-decisions.md` on auth, setup tokens and password confirmation.

**Look hardest at:**
- **Coverage:** any route that mutates state or returns user data without the guard, including SSE `/events`, `/logs`, exports, downloads and the feed or RSS tokens.
- **Comparisons:** compares of secrets, keys and password hashes that aren't constant-time.
- **The setup token:** can it be reused, does it expire, and is there a race when two requests use it at once?
- **The login limiter and rate limits:** bypasses through header spoofing, or memory growth without a bound.
- **Proxy headers:** `X-Forwarded-For` trusted from untrusted peers, and the walk order.
- **Keys:** revoke and delete really stop the key working, including any key already cached, and key IDs can't be enumerated.
- **The logs:** any secret written to them (§12.1 says secrets never appear in logs).

---

## Area 3: content filtering and adult detection

**Scope:**
- **`crates/filter/src`:** `core.rs`, `engine.rs`, `matcher.rs`, `normalize.rs`, `rules.rs`, `prefilter.rs`, `held.rs`, `seal.rs` and `reclassify.rs`.
- **`crates/classify/src`:** `adult.rs`.

**Spec:** architecture §20 (all of it), plus `docs/owner-decisions.md` §1.8 and the content-safety items in §2.

**Look hardest at:**
- **Normalisation:** inputs that slip past it and so escape a rule. Unicode confusables, zero-width characters, full-width forms, combining marks, mixed scripts, leetspeak, and separators such as `.`, `_` and `-`.
- **Prefilter versus full matcher:** any case where the cheap prefilter says "no match" but the full matcher would say "match". That's a false negative, and it lets content through.
- **`NEAR`, negation, `unless` and exceptions:** cases where they release more than intended.
- **The child-safety core:** can a user rule, an override or "Always allow" ever weaken it? The owner decisions say it can't be weakened.
- **The quarantine:** sealed names can leak in a decoded form, through logs, the API, exports or events.
- **Adult detection:** a torrent with the "Show adult content" setting off can still reach search, facets, Discover or the SSE counts.
- **Reclassification races:** a torrent visible for a moment before its filter verdict applies.
