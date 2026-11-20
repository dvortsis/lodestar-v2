# ENR-tmdb landing (Opus; runner brief 2026-11-20)

The API lane built #948 ENR-tmdb (branch `slice/ENR-tmdb`, head 5e023193) and has now stopped. The code passed its security review. Its review said CHANGES only because of the recorded merge order (X-snapshot-claims-retention first). That merged as #976, so the order is met. The PR conflicts with main.

## Steps
1. Check out `slice/ENR-tmdb` and run `git fetch origin main`. **Merge** origin/main into it. Never rebase, amend or force-push.
2. Resolve the conflicts, keeping every entry from both sides. Expect shared plugin lists, manifests, credential_spec, fuzz/README.md, the lockfile and the runner yaml. Since the PR was written, the snapshot claims and retention work (#976, #980, #987) has also changed `crates/store/src/snapshots*`. If TMDB touches snapshot terms or claims, adapt it to the merged API and say exactly how.
3. Run the full CLAUDE.md checks for the crates touched, and the status-connector `npm test` if you touched the yaml. Push once.
4. Send "ready 948 <full sha>" with the list of conflicted files and how you resolved each one. A fresh delta re-review follows.

## Not in scope
- Important 1 from the review, gating the TMDB logo and recorded fixtures on the wiring slice, is a follow-up the runner queues. Don't build it here unless the merge forces it.
- No live calls to TMDB.

## Owns
`slice/ENR-tmdb`, the files the PR already changes, and whatever the conflicts force you to touch. Name those files.
