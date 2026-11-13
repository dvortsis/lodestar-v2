VERDICT: MERGE

# Review: PR #825, X-gluetun-templates-hostfile, head 9fffec07

Fresh independent code and security review (/security-review standard). Head 9fffec07 against merge-base 667479cb: 22 files, +1414/−21. Bar: CLAUDE.md, `runner/standards.md`, the brief `briefs/X-gluetun-templates-hostfile.md` and its slices.yaml notes, and the runner calls already accepted.

## What I ran
- `cargo test -p lodestar --bin lodestar -- companions:: privdrop::`: 63 passed, 0 failed, 0 ignored (as root, in the sandbox).
- The same native tests under `umask 002`: 3 failed (see I2).
- Diff scans: no `sleep`, `#[ignore]` or `cfg_attr` skips. No 12-digit numbers, IP addresses, model ids in code or docs, CI run ids, personal data, hex colours or non-UTC times. The only model names are the harness-supplied Co-Authored-By trailers.
- The last commit (9fffec07), checked as claimed: `RULES` is now `#[cfg(unix)]`, and `openat(O_PATH|O_NOFOLLOW)` became `statat(AT_SYMLINK_NOFOLLOW)`. On Linux and macOS, a symlink at `post-rules.txt` now yields the link's own status (mode 0777, so `locked()` is false), and any errno gives `false`. `found()` then refuses the link anyway (ELOOP). Correct on both, and fails closed.

## Security review summary (no blocking findings)
- **Contents:** the file's bytes are only the compile-time `GLUETUN_POST_RULES`. No user or network value reaches it, so iptables, newline or comment injection isn't possible.
  - The user-controlled `companions_dir` reaches only the Unraid XML `Default`/value, after `HostPath::parse`: absolute, `[A-Za-z0-9._ -]` only, no `.`, `..` or empty components, and the forbidden list. quick_xml escapes it.
- **Container write (privdrop, root, before the drop):** fd-based throughout.
  - `/companions` is opened `O_DIRECTORY|O_NOFOLLOW` and its only parent is `/`.
  - `gluetun` uses `openat`/`mkdirat` with `O_NOFOLLOW`.
  - The file is written as a temp file (`O_CREAT|O_EXCL|O_NOFOLLOW`, random name), then `fchmod 0644`, `sync_all`, `renameat` in the same folder and a folder fsync. The temp file is unlinked on failure.
  - Owner and mode are enforced on `/companions` and `gluetun` (root, no g/o write), so the dropped process can't swap either. The check-to-write TOCTOU is closed because only root can change a locked folder.
  - A pre-existing directory at `post-rules.txt` (what Docker leaves) is not replaced, so gluetun stays down (fails closed).
- **Template-time read (dropped):** `saved_by(..,0)` requires both folders and the file to be root-owned with no g/o write, a regular file, at most the rules plus 1 KiB, and byte-equal. Anything else falls back to plan A.
- **Fail-closed:** the rename is atomic, so gluetun sees the old file or the new one, never a partial one. A missing file makes Docker create a directory, and gluetun stops.
- **Errors and logs:** the structured log carries the fixed `/companions` path and a kind only.
- **Platform:** the Windows stubs (`saved_rules`, `save_native`) and test gating look right, and CI is reported green on Windows. privdrop's non-Linux `imp` never calls the writer.

## Blocking
None.

## Important

### I1 [SEC]: a host folder nested inside a folder another principal can write isn't detected, and the decision record says it is
- **Where:** `crates/lodestar/src/privdrop.rs:774-784` (the data-overlap checks in `save_companions_rules`). Also `docs/decisions/2026-11-13-X-gluetun-templates-hostfile.md`, the lines "A mount of a whole share, or of the data folder, is never written to" and "This also covers a /companions mapped onto /data or a companion's folder (fresh review I3): privdrop leaves such a folder alone".
- **The gap:** privdrop compares `/companions` with `/data` only by `(st_dev, st_ino)` and by in-container canonical path (`/companions` against `/data`). Neither can see that the host folder behind `/companions` is a subfolder of the host folder behind `/data`, or of a companion's folder.
- **Attack path:**
  1. An existing install adds the mount, so `/data` is already PUID-owned and `chown_data_dir` skips its tree walk.
  2. The user picks a host companions folder inside a data folder not named `appdata/lodestar`, such as `/mnt/disk1/lodestar/companions`. In a container `input.data` is `DataFolders::none()`, so the template-time name check can't catch it either.
  3. Docker creates the subfolder as root 0755, privdrop writes the root-owned rules, and `saved()` passes.
  4. A compromised, dropped Lodestar owns `/data` (the same host folder). It renames `/data/companions` and creates its own `companions/gluetun/post-rules.txt`. That rename works because the bind source isn't a mountpoint in Lodestar's namespace.
  5. On its next start, gluetun runs the attacker's rules as root, for example an ACCEPT rule that defeats the kill switch.
- **A second route:** a folder inside a companion's downloads or config folder that the request doesn't name (`taken` is always empty in `Service::input`) lets that companion do the same.
- **Severity:** both routes need the user to choose a nested path against the template and guide wording, plus a compromise. The defaults are safe, and plan A has the same exposure for a user-chosen path. So this is not a regression and not blocking, but the decision record overstates the protection.
- **Fix:**
  - (a) In privdrop, read `/proc/self/mountinfo`. When the `/companions` and `/data` mounts share `major:minor` and the root field of `/companions` lies inside that of `/data` (or the reverse), leave it with kind `data`. This is cheap and covers the data-folder case at any depth.
  - (b) In the template `Description` and guide ch. 3, say to keep it as a folder of its own in appdata, not inside the Data folder or any other container's folder.
  - (c) Correct the two sentences in the decision record so they match what is actually detected.

### I2: the native folder is created with a umask-dependent mode, then refused as `companions_dir_shared`
- **Where:** `crates/lodestar/src/companions.rs:1991` (`std::fs::create_dir(root)` in `post_rules::save_native`).
- **Failure:**
  1. `create_dir` uses mode 0777 & ~umask. Under umask 002, the default for users on Ubuntu and Fedora, the folder is 0775.
  2. `safe(&r)` then refuses it with `companions_dir_shared`, the folder Lodestar has just created is left behind, and every later request is refused until the user runs chmod.
  3. Fails closed (no security impact), but the native feature doesn't work for those users. `gluetun_dir` already does the right thing (`fchmod 0755` after `mkdirat`); the outer folder doesn't.
- **Reproduced:** `umask 002; cargo test -p lodestar --bin lodestar -- companions::tests::natively` fails `natively_only_a_named_folder_is_used` with `BadInput { companions_dir, companions_dir_shared }`. Two more tests fail because they build fixtures with `create_dir_all` under that umask. So the tests also depend on the runner's umask.
- **Fix:**
  - Create with `DirBuilder::new().mode(0o755)`, then `fchmod 0755` through the opened fd only when this call created it, mirroring `gluetun_dir`.
  - Set explicit modes in the native tests' fixtures, or set the umask in them.
  - Optionally, remove a folder this call created when it then refuses.

### I3: the API contract text describes a native default that the code removed
- **Where:**
  - `crates/api/src/dto/companions.rs:271-272`: "natively, a folder outside the data folder (a sibling of it, `lodestar-companions`, by default)".
  - The same text, regenerated, in `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts`.
  - `crates/lodestar/src/companions.rs:1298-1299`: "`companions_dir`, or `lodestar-companions` beside the data folder".
- **Failure:** the code (and the decision record, "Only a folder you name") uses no default natively: an unset `companions_dir` returns `None`. The follow-up UI slice X-gluetun-hostfile-ui reads the OpenAPI description and would expect a default that doesn't exist.
- **Fix:** say "natively, a folder you name outside the data folder; unset, you save the file yourself". Regenerate openapi.json and schema.d.ts, and fix the rustdoc.

## Nit
- **N1 [SEC]:** `companions.rs:1980-1995`, native intermediate components. The parent is checked with `canonicalize` and then reached again by path (`create_dir(root)`, `open(root, O_NOFOLLOW)`), and only the last component is no-follow.
  - Between the two, someone who can write an ancestor could swap in a symlink. The native trust model makes this low-risk.
  - Fix: on Linux, open the parent with `openat2(RESOLVE_NO_SYMLINKS)`, or walk it with `openat(O_NOFOLLOW)` per component. Then `mkdirat`/`openat` the leaf from that fd.
- **N2:** `companions.rs:3899`. `in_a_container_only_a_saved_file_is_used` asserts only under a condition, and nothing when `/companions` exists with the rules. The positive container path is covered only through `saved_by`. Either inject the companions root, or rename the test to say what it checks.
- **N3:** `privdrop.rs:799`. The `foreign` reason puts a raw directory entry name (`from_utf8_lossy`) into the human-readable line. Only root can create entries in a qualifying folder, so the risk is low. Escape control characters (`escape_debug`) or drop the name.
- **N4:** guide ch. 3. If gluetun was ever pointed at the saved path before the file existed, Docker leaves a directory there, and privdrop then refuses to write (kind `write`). The guide's "If the file is missing" line could say to delete that directory (`rm -r .../gluetun/post-rules.txt`) and restart Lodestar.

## Checked and fine
- **Ownership:** the files are in the brief's owns plus the runner's §2.33 additions, and the decision record lists them.
- **Event names:** both are in `events.rs`, the golden catalogue, `system.ts`, and en.json and zh.json (both present, sensible zh). No UI component strings, no hex values.
- **User docs:** chapter 3 recommends the safe default (let Docker create the folder, no New Permissions, keep it outside Data), gives a `sha256sum` and `stat` check with an expected result, and keeps plan A as the fallback. operations.md §4.3 states the native caveat.
- **Callers:** `ServiceInput.gluetun_paths` is wired through every caller (the CLI sets `None`, the flaresolverr tests updated). The Unraid gluetun template switches its value, help and Overview text, and compose is unchanged (tested).
- **Native write timing:** it happens only after the template generates (`a_refused_template_saves_nothing`), and a file that differs is never overwritten natively.
