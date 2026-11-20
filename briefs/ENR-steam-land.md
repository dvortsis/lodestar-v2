# ENR-steam landing (Opus; runner brief 2026-11-20)

The API lane built #969 ENR-steam (branch `slice/ENR-steam`, head 63538717) and has now stopped, so no other session owns that branch. A fresh Opus review of 63538717 says MERGE. The PR conflicts with main, so its CI hasn't run.

## Steps
1. Check out `slice/ENR-steam` and run `git fetch origin main`. **Merge** origin/main into it. Never rebase, amend or force-push.
2. Resolve the conflicts. Expect shared lists: the plugin catalogue (`catalogue.rs`, where #986 just moved igdb after full_scrape), `tests/manifests.rs`, credential_spec.rs, fuzz/README.md, Cargo.toml and Cargo.lock, `runner/slices.yaml` and `runner/dashboard.yaml`, and the decisions index if there is one.
   - Keep every entry from both sides, in the order main's comment asks for.
   - Regenerate lockfiles and generated files with the repo's tools, never by hand.
3. Run the full checks CLAUDE.md lists:
   - `cargo fmt --check`;
   - `cargo clippy --workspace --all-targets -- -D warnings`;
   - `cargo test --workspace`, or at least the sources-plugins, sources-feeds and fuzz-adjacent crates plus anything the conflicts touched;
   - `cd tools/status-connector && npm ci && npm test`, if you touched the yaml.
4. Push once.
5. Send the runner "ready 969 <full sha>" when CI is green. List each conflicted file and how you resolved it, so the runner can run a delta review of the merge.

## Not in scope
- No new features. The review's Important items are follow-ups the runner queues:
  - refusing a finished list that is empty or much smaller than the current one;
  - an `ext:` condition for the adult sign-off, a live terms re-read and Valve accepting `x-webapi-key`;
  - tests for the 60 s cancel hold and the partial-list age limit.
- Mention any of these you notice break during the merge, but don't build them here.
- No live calls to Steam or any other service.

## Owns
`slice/ENR-steam` and the files the PR already changes, plus whatever the conflicts force you to touch. Name those files in your ready message.
