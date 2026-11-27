# D-start: user and packaging docs, no start wait and safe restart on by default

**From:** the runner, 2026-11-27T20:19Z. **Model:** Sonnet (docs, per the delegation policy). **Branch:** `slice/D-start`. **PR title:** `D-start: User docs say sampling starts at once and safe restart is on by default`.

**Why (owner, 2026-11-15, `docs/owner-decisions.md` §2.77; `docs/decisions/2026-11-15-owner-no-start-wait.md`):** CR-0w has merged.
- **No sampling wait after any start:** first installs, crashes, restored backups, unproven restarts and image updates all sample as soon as the DHT has bootstrapped.
- **Safe restart is on by default.** After a proven clean restart, it restores per-node restrictions so the node doesn't re-ask nodes it already has samples from. When the proof fails, the node samples at once.
- **The docs still describe the old behaviour.** They tell people to expect a six-hour wait, and call safe restart an optional switch.

**Read first:**
- `.claude/skills/user-docs/SKILL.md` and the owner's `github-docs` skill it names;
- `docs/decisions/2026-11-15-owner-no-start-wait.md` (all of it; "Proposed slices" item 4 is this slice);
- the D-start entry in `runner/slices.yaml`;
- CR-0w's decisions file (`docs/decisions/*CR-0w*.md`), for what was actually built.

Describe what the code on `main` does. Where the decision and the code differ, the code wins; tell the runner.

**The guide is written for v0.5.1.** Follow its existing convention: development behaviour is marked **(Newer than v0.5.1)** beside what v0.5.1 does (see `docs/user/03-keep-lodestar-safe.md:77`, `:181`, `:411`). v0.5.1 still waits six hours after a start, and its safe-restart switch isn't available. So keep the v0.5.1 facts, and add the newer behaviour marked that way. Don't delete what's true for v0.5.1.

**The work**
1. **Section 3.8 is safe restart again (runner, 2026-11-27).** The Legal Notice (section 5) points to "user guide, section 3.8" for the safe-restart check's restart record. Today 3.8 is "Check your understanding". So:
   - Restore a safe-restart section as **3.8**, **(Newer than v0.5.1)**. Cover: what it does; that it's on by default; the restart record folder outside the data folder (Linux `/var/lib/lodestar-witness`, Windows, and Docker, where the record stays inside the container so an image update loses one run's restored restrictions); keeping that folder out of backups taken with the data folder, and why; and that a failed proof just means sampling starts at once.
   - Renumber "Check your understanding" to **3.9**, and fix every link and index entry that points at either.
   - Use `git log -S "3.8" -- docs/user/03-keep-lodestar-safe.md` to find the old 3.8 text (revision 1.3). Reuse what's still true.
2. **Every six-hour-wait and safe-restart-off line** in your owns. Find them with `git grep -n -i -E "6 hours|six hours|6-hour|six-hour|safe.restart|suppress"`. Starting points: `02-…docker.md:249`, `03-…:308`/`:313`/`:320-326`, `04-…linux.md:235`, `05-use-lodestar.md:57,69`, `06-troubleshooting.md:14`, `appendix-b-settings-reference.md:33-36`, `docs/operations.md`, `packaging/docker/README.md:150-156`, `packaging/systemd/README.md`, `docs/ui-glossary.md` and `README.md`. Give each the v0.5.1 fact plus the newer behaviour.
   - **Expectations:** the index grows over days and weeks (`docs/architecture.md` §3, "Cold-start reality"). Don't promise speed.
3. **`docs/user/revisions.md`:** add a row (version 2.x, today's date) saying what changed, including the 3.8/3.9 renumbering.
4. **Gates:** the user-docs skill's checks (links, index, nav, and any docs lint CI runs). Run the repo's docs checks locally. If a gate needs a screenshot you can't take, say so; never fake one.

**Owns:** `docs/user/02-install-lodestar-on-unraid-and-docker.md`, `docs/user/03-keep-lodestar-safe.md`, `docs/user/04-install-on-windows-and-linux.md`, `docs/user/05-use-lodestar.md`, `docs/user/06-troubleshooting.md`, `docs/user/appendix-b-settings-reference.md`, `docs/user/revisions.md`, `docs/user/index.md` and `docs/user/glossary.md` (links and entries only), `docs/operations.md`, `packaging/docker/README.md`, `packaging/systemd/README.md`, `docs/ui-glossary.md`, `README.md`, `docs/decisions/*-D-start.md`.
- Leave `runner/` and the Legal Notice alone. X-legal-safe-restart is changing the notice now, with the owner's words.
- Ask the runner before touching any other file.

**Size:** docs only, probably 200–400 lines.
