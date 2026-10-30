# CR-0d: restart without the 6 h sampling suppression, via a crash-safe protocol
**Model:** Opus (crawler core, on-disk state, BEP 51 obligations). Runner brief, 2026-10-30. Needs an Opus protocol review before merge.

## Base and timing
- **Phase 1 (design) starts now** and is docs only. **Phase 2 (code) starts after CR-0f (#200) is on `main`**, because CR-0f changes the DHT actor's bootstrap and pause paths. The runner tells you when.
- Branch `slice/CR-0d` from `origin/main`. Never use an older clone.
- It merges behind a runtime switch, **off by default**. It isn't in v0.5.1 unless the release delta review says so.

## Contract
- **Master plan Part A3** (`docs/decisions/2026-10-30-master-plan.md`, "CR-0d"). Each of the following means **full suppression**:
  - a run that loaded a snapshot without first durably (fsync) invalidating the clean marker before its first send;
  - a backward clock jump, or an implausible gap;
  - a missing, corrupt or version-mismatched snapshot file;
  - a different data-directory identity or node identity;
  - a restored backup, i.e. a generation that isn't newer than the last seen.
- **Shutdown order:** stop new samples first. Then outstanding requests either settle or are recorded as obligations until their timeout plus the remote interval. Only after that are the snapshot and marker written atomically, with a version, a checksum and a generation.
- **Clock policy:** obligations are wall-clock deadlines.
- **On a clean restart,** sampling resumes but every restored restriction is honoured. It must not "sample immediately".
- **BEP 51** sets the obligation: the remote's interval. CR-0's 6 h suppression exists because the node can't prove it honoured intervals across restarts.
- **First install:** CR-0d can't help it. The first start stays suppressed (an owner product choice, recorded).
- **Read:** the CR-0 code (the sampler and suppression in `crates/dht/src/node/`, especially around the suppression logic near `actor.rs` ~1711 and the snapshot code), `docs/plan/2026-10-27-baseline-execution.md`, and the CR-0 soak evidence on `main` (`docs/soak/evidence/2026-10-30-cr0-final/`).

## Phase 1: design (docs only, then the runner's OK and an Opus protocol review)
Write `docs/decisions/<date>-CR-0d.md` with:
1. **The on-disk format:**
   - the marker file and the snapshot file: their fields, version, checksum, generation, and data-dir and node identity;
   - the atomic write (temp, fsync, rename, directory fsync), and what happens on every crash point.
2. **A state machine:** start → load → validate → invalidate marker (fsync) → first send → … → shutdown quiesce → settle or record obligations → write snapshot + marker. Give the crash outcome at every arrow (full suppression or safe resume).
3. **Obligation semantics:**
   - per remote node or per sample target;
   - wall-clock deadlines;
   - behaviour under a clock jump;
   - the size bound of the obligation table and its pruning.
4. **Pause/resume interaction,** after CR-0f lands: is pause a shutdown for this protocol's purposes?
5. **The switch:** its name, default off, the exact off behaviour (today's), and the metrics (suppression reason on start, obligations restored and honoured).
6. **The test plan:**
   - a crash at each step: kill -9 between fsyncs, using a fault-injection seam under cfg(test);
   - a reload → send → crash → reload loop;
   - clock skew forward and backward;
   - corrupt, truncated and foreign files;
   - a backup restored (older generation);
   - a CR-SIM scenario showing restored restrictions are honoured.

   No fixed sleeps: use paused time.

Message the runner a short summary and wait for the OK before any code.

## Own
Phase 1: `docs/decisions/<date>-CR-0d.md`. Phase 2 (named after the OK): the DHT sampler/suppression module, the new persistence module, its tests and the switch plumbing.

## Done when
The design is OK'd and protocol-reviewed. Phase 2 passes fmt, clippy (`-D warnings`) and `cargo test --workspace`, and has a fresh-reviewer self-review plus the Opus protocol review.
