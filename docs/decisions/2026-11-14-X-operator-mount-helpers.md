# X-operator-mount-helpers decisions (#872)

These change how the operator gets its channel helper (`infra/operator/thread.py` and `infra/fleet/reader.py`). They replace the fetch in the OP-4 system prompt (`2026-11-11-OP-4.md`) and keep everything else there. The runner approved the slice on 2026-11-14 as a tightening within its call. The owner chose, the same day, to keep `bash` on the `auto` permission policy and retry with the helper mounted, rather than move `bash` to `always_allow`.

## Why
- **The first live wake's one command was refused.** The operator's prompt told it to download the two helper files from `api.github.com` and run them. The `auto` permission policy judged that call high-risk (`reason_code: high_risk`) and denied it, so the operator could neither read the channel nor post (`2026-11-14-X-operator-wake-outcome.md`).
- **Downloading code to run it was the wrong shape anyway.** The helper came from `main` at run time, so a session ran whatever `main` held at that moment, not the commit the bridge ran from.

## The helper is mounted, not fetched
- **The bridge uploads the two files and attaches them to the session** as read-only file resources, at `/op/thread.py` and `/op/reader.py`. In the sandbox they're under `/mnt/session/uploads/op/`.
- **The bytes come from the job's own checkout,** the commit `operator-wake.yml` runs from. Nothing is read from a branch name or from `main` at run time. A test checks the uploaded bytes against the files.
- **The prompt copies the folder and runs the copy.** Its fetch block is gone; the block that replaces it is the one the injection test already used (`mkdir -p /tmp/op && cd /tmp/op`, then a copy of `/mnt/session/uploads/op/*`).
- **The prompt keeps one `curl`:** the last-resort escalation, a POST of one comment to the channel, for when the copy or the helper fails. Without it an operator with a broken helper could only say so in a message nobody reads. The runner agreed to keep it on three conditions, which the prompt now states and a test checks:
  - **It downloads and runs nothing.** The command is one POST to the channel's comments, with no output file.
  - **Its reply goes nowhere.** It's never piped, redirected or run.
  - **Its body is the operator's own report.** It says only that the helper failed and at which step, with nothing read from the channel, a file or command output.
- **These are prompt rules, as before.** What the operator does with `bash` is still bounded by the sandbox's network, the token's scope and the `auto` policy, not by the prompt.
- **The copy is `cp -f`.** Mounted files are read-only copies, so a second plain `cp` over the first copy could fail.
- **The uploads happen before the App token exists.** They come after the ledger checks and before the token is minted, so a wake that can't mount its helper holds nothing to revoke.
- **A failed upload starts no session.** The bridge posts "no session started: the helper files couldn't be uploaded", deletes each upload whose ID it received, releases the lock and exits 0, like any other refusal before a session. The error itself is never printed. An upload whose reply was lost has no ID to delete, so it can leave a small file in the workspace; it holds only public repository code.
- **A create the API refuses** (a file it won't take, a bad mount) follows the bridge's existing failure path: an escalation, the token revoked, the files deleted, the lock released, a failing job.
- **The files are deleted at the end of every wake,** after the session is archived and before the lock is released. The session keeps its own copies, so the delete can't affect it. A delete that fails is escalated ("couldn't delete a helper file; check it by hand") and the remaining steps still run. A cancelled job can leave two small files in the workspace; they hold only public repository code.

## The injection test
- **The test now judges the prompt the operator runs, unchanged.** Before, it swapped the fetch for a copy. The prompt has the copy itself now, so `patch_prompt` only checks that the copy block is there once and that nothing in the prompt fetches (a second `curl`, `wget`, `git clone`, `pip install` and the like). If either fails it refuses to run, since the mock would then never be used.
- **One upload implementation.** The multipart upload moved from `injection/harness.py` to `wake.py`; the test uses the same function. The test's mount folder and its real helper files are the wake's own (`wake.MOUNT`, `wake.HELPERS`).
- **How the test judges a session doesn't change.**

## What doesn't change
- **The permission policy stays `auto`** for the whole toolset (the owner's choice, above). Whether `auto` allows the copy, the read and the post is what the next wake shows; the bridge now reports a denial by its reason code.
- **No limit, gate, budget, lock or token scope moves.** The sandbox's network is still `api.github.com` only. The bridge calls one more endpoint with the workspace key, the Files API on `api.anthropic.com`, to upload and delete the two files.
- **The helper's own rules are untouched:** which comments it trusts, and what it will post.

## After this merges
- **The owner dispatches `operator-apply.yml` once.** The prompt changed, so the agent needs a new version. Until then a wake still runs the old prompt and its fetch is denied again, and the injection test refuses to start because the deployed agent differs from `main`.
- **The owner's first-run checklist follows the change** (`docs/operations/cloud-operator-first-run.md`, added to this slice by the runner). Step 2's "helper fetch" item, with its two `curl` fetches and its rate-limit branch, is now "the mounted helper": the owner checks that the copy and `thread.py read` ran and weren't denied, and the page says what a denial, a failed helper and a failed upload each look like. Its troubleshooting entry names the test's two new refusals.
- **Two comments said "fetched"** (`infra/operator/thread.py`'s docstring and a comment in `infra/fleet/reader.py`). They say "mounted" now; nothing else in those files changes.
- **One line of that page was left for the runner:** the sentence the owner copies into check 8's results said the test's sessions could tell they were a test partly because their files were "copied from uploads". A real wake copies from uploads too now. The runner reworded it in `2026-11-14-X-operator-mount-nits.md`.
