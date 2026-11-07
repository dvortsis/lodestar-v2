# WK-8: What a worker judges with (filter and dictionary snapshot, judged before anything is sent or spooled)
**Model:** Opus, with `/security-review` (content safety). A fresh Opus reviewer follows. Runner brief, 2026-11-07T06:25Z. It takes the slot WK-3 freed (#566 merged), within owner §2.42's four slots.

## Spec
- **The plan:** `docs/plan/2026-11-06-v06-workers.md`. Read:
  - the WK-8 row and paragraph in §4.3, **which is your spec and gates**;
  - **Q1 (line ~182). It is decided: the plan's recommendation, as its plan row gives it** (owner §2.46 and §2.47; the owner confirmed "Sealed names kept on worker disks" by name). `ext:owner-wkq1` holds.
- **Architecture:** §19.4, §19.6 and §20 (the actions and sealing). Owner §2.17 covers secure deletion.
- **`runner/slices.yaml`:** the `WK-8` entry.
- **What has merged:**
  - WK-0 (#538): the chunked messages;
  - WK-5 (#560): `Assign` carries the dictionary and filter-snapshot versions;
  - WK-6 (#539): the Results codec, with the worker's verdict and rule version;
  - WK-7 (#549): `WorkerStages` and `Preparer::from_dict`;
  - WK-3 (#566): have-filter replication, the pattern for a chunked, versioned snapshot. Reuse its approach to headers and chunk assembly where it fits.
- **Running alongside: WK-9** (#568, coordinator intake). It already refuses an unknown or retired dictionary id per record at commit (`Preparer::has_dict` in `crates/store`). Build on it, don't duplicate it. If your gate needs a change there, ask the runner, and the WK-9 session will make it. Don't touch `crates/pipeline/src/remote.rs`, `writer.rs` or `crates/store/**`.

## Do
Follow Q1 and §4.3:
1. **The versioned filter snapshot** (`crates/pipeline/src/wire/filter_snapshot.rs`) holds exactly what the commit judges with: the locked floor, the CS-1 entries and signatures, switches, packs, the user's rules, and classification rules.
   - The encoding is bounded: counts, lengths and total bytes are capped before allocating, unknown versions are refused, and nothing half-used.
   - Chunked over WK-0's messages.
   - A fuzz target: `fuzz/fuzz_targets/cluster_filter_snapshot.rs`, plus one `[[bin]]` line in `fuzz/Cargo.toml` and one row in `fuzz/README.md`.
2. **The coordinator side** (`crates/lodestar/src/cluster/filter.rs`):
   - build the snapshot from the store and send it on change;
   - send the current file-tree dictionary (the store's `file_dict`), versioned;
   - one `mod` line in `cluster/mod.rs`.
3. **The worker side** judges every prepared torrent before it is sent or spooled. Expose a judge handle that WK-10 plugs into the stages; it isn't wired into a runtime yet. Per §20 action:
   - **drop** (hash-list matches): dropped on the worker and counted, never sent or spooled;
   - **quarantine, hide, demote, flag and the adult hold:** only marked, so the coordinator applies them at commit.
   - A quarantine-marked record's name is **sealed** on the wire and in the spool (the spool is WK-11's; give it the sealed form).
4. **A stale snapshot is re-judged at commit.** The coordinator's commit already re-judges every remote result, and WK-9 keeps that. Your test shows that a worker judging with an old snapshot can't get a record past today's rules.
5. **The decisions file:** `docs/decisions/2026-11-07-WK-8.md`, with notes for WK-10, WK-11 and WK-13. Include the snapshot's size at defaults and with large user rule sets.

## Done when
- **§4.3's gates pass, each with a test that fails without the change:**
  - a blob under an unknown dictionary id is refused at commit. WK-9 already covers this, so cite its test, or add one that runs through your snapshot path;
  - a drop-action fixture prepared on a worker never reaches the wire or the spool;
  - a quarantine-action fixture reaches the coordinator's quarantine with its name sealed throughout;
  - a stale snapshot is re-judged at commit;
  - the decoder fuzz target exists.
- **The sealed name never appears in logs, `Debug` output or error text.**
- **Single-machine parity:** with no worker joined, nothing is built or sent.
- **No fixed sleeps**, and no tokio `test-util` or `testing` features on the `lodestar` package.
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- **Size:** about 800 lines (the runner splits anything over about 3k). Ask the runner before editing outside your owns and the one-line shared edits named above.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `WK-8: What a worker judges with`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green.
