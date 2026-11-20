# X-operator-mount-nits decisions (#876)

Wording only, from the runner's review of #872 (`2026-11-14-X-operator-mount-helpers.md`). No code path changes.

- **The runner reworded the sentence the owner copies into check 8's results.** It came from `2026-11-14-X-operator-hardening.md` ("One neutral line in the results") and listed "files copied from uploads" as a sign that a session was a test. Real wakes copy from uploads too now, so the list reads: an extra helper file in the mounted folder, a helper that works without the network, fixed dates, a readable fixture. The rest of the sentence is unchanged.
- **The first-run checklist's "Denied" case says "denied N of its tool calls".** A denied copy is often followed by a denied fallback `curl`, so the count is often 2. It also names all three reasons the bridge can print: `high_risk`, `indeterminate` and "an unlisted reason".
- **The mount-helpers file is corrected in place,** on the runner's word, since it's the same change's own record: a failed upload deletes each upload whose ID the bridge received, and an upload whose reply was lost can leave one small file of public code.
- **The harness's docstring no longer says the prompt's fetch is swapped for a copy.** The prompt is used unchanged.
