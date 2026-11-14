# X-gluetun-templates-hostfile decisions

Lodestar now saves gluetun's post-rules file itself, in a companion-files folder, and gluetun's Unraid template can mount that file. This follows the runner's brief `briefs/X-gluetun-templates-hostfile.md` and the runner's answers of 2026-11-13. These defaults are a runner §2.19 flag to the owner. The guide's step 1, where the user saves and checks the file, stays as the fallback; it was plan A in X-gluetun-templates (`2026-11-12-X-gluetun-leak.md`).

## Where the file lives
- **A companion-files folder of its own, never Lodestar's data folder** (runner). Mounting a file from Lodestar's appdata into gluetun would be an exception to the rule that companions never mount Lodestar's own folder, which holds its secrets (§18.10, `packaging/companions/README.md`). Weakening that rule is the owner's call, so the file goes elsewhere.
  - **Unraid:** Lodestar's templates (`packaging/unraid/lodestar.xml`, `lodestar-gluetun.xml`) gain an optional **Companion files** Path. Its default host folder is `/mnt/user/appdata/lodestar-companions`, mounted at `/companions`. It holds one small file, so `/mnt/user` is fine.
  - **The file:** `<companion files>/gluetun/post-rules.txt`. gluetun mounts it read-only as a single-file bind.
- **`ServiceInput.gluetun_paths`** (`GluetunPaths { companions }`) carries the host folder.
  - **Unraid's gluetun template** is the only one that reads it. With it, the **Post-rules** Path points at the saved file and the Overview says Lodestar saved it. Without it, the Path keeps plan A's default and the save-it-yourself note.
  - **Compose** carries the rules inline either way.
  - **The CLI** generates compose only, so it sets none.
- **The folder holds nothing a companion could change** (`GluetunPaths::check`). gluetun runs every rule as root, so these are refused:
  - a folder that overlaps the running data folder, where it's known;
  - Unraid's `appdata/lodestar` or `appdata/lodestar-worker`, which a container can't see;
  - the routed companion's own settings or downloads folder (fresh review B1);
  - another companion's known folder, or any catalogue companion's `appdata/<name>`;
  - a dot component.
- **The API field:** `TemplateRequest.companions_dir`, the host folder. It's read only for an Unraid template that includes gluetun.
  - **The UI field** is the follow-up slice X-gluetun-hostfile-ui (runner).
  - **The new refusal codes** for that slice to map are `inside_data_folder`, `inside_other_companion`, `dot_folder`, `bad_template_input`, `companions_dir_link`, `companions_dir_shared` and `companions_dir_unwritable`, all on `companions_dir`.

## privdrop writes post-rules as root (supersedes the chown handover the runner approved)
The first build handed `/companions` and `/companions/gluetun` to PUID:PGID, and Lodestar wrote the file after dropping root. The security review found that this let the dropped process rewrite `post-rules.txt`. gluetun runs every `iptables` line in it as root with NET_ADMIN on its next start. So code execution inside Lodestar, which parses untrusted DHT, KRPC and metainfo data, would become control of gluetun's firewall: an ACCEPT rule would leak the real IP and defeat the kill switch the file exists to close. Plan A never gave Lodestar that reach.

The runner chose the stricter design (runner, 2026-11-13, §2.33): nothing the dropped process runs can change what gluetun runs as root.

### At start-up (privdrop, still root)
`save_companions_rules` in `crates/lodestar/src/privdrop.rs` writes the fixed embedded rules, root-owned, mode 0644, in a root-owned 0755 `gluetun` folder. It does this only inside a `/companions` that:
- is a real folder, not a link and not `/`;
- is apart from `/data`, both by identity (`st_dev`/`st_ino`) and by canonical path in both directions;
- is owned by root with no group or other write bit;
- holds nothing but `gluetun`.

A mount of a whole share, or of the data folder, is never written to.
- **Why the owner and mode rule** (second security review F1, F2): a user who can write `/companions` could rename the root-owned `gluetun` folder after start-up and put their own in its place. gluetun names the file by its host path, so it would mount theirs on its next start. Docker creates a missing host folder as root, 0755, so the default passes. A folder made over a share, or one Unraid's New Permissions changed, falls back to plan A, and the guide says so.
- **The `gluetun` folder:** it's created with `mkdirat` and set to 0755 only when it's missing; one someone else creates in between is never changed. One that exists must be a real folder owned by root with no group or other write; otherwise it's left alone.
- **The write:**
  - a temp file created with `openat(O_CREAT|O_EXCL|O_NOFOLLOW)`, then `fchmod` 0644 whatever the umask;
  - fsync, then `renameat` over `post-rules.txt`, then an fsync of the folder;
  - the temp file is removed on failure.
- **Links and old files:** `renameat` replaces a link or a hard-linked file rather than writing through it. A root-owned file that differs (an upgrade that changes the rules) is replaced. An exact file is left in place.
- **What it never does:**
  - chown, chmod or delete anything it didn't create;
  - create `/companions`;
  - stop start-up.
- **One log line:**
  - `system.companions_dir` when the rules are in place (`changed`: 1 written, 0 already there);
  - `system.companions_dir_left`, with a kind only, when they aren't.

  privdrop's report holds the line until logging starts.
- **The ownership check is injectable** (`owner`), so the logic is tested without root. Production passes `geteuid()`, which is 0 there.

### At template time (container)
Lodestar only reads. It points the template at the file only when every one of these holds:
- `companions_dir` is given;
- `/companions` and `/companions/gluetun` open as real folders (`O_NOFOLLOW`), both owned by root with no group or other write;
- the file is a regular root-owned file within a size cap of the rules plus 1 KiB;
- the file is byte-equal to the embedded rules.

Anything else falls back to plan A.

### What the container side can and can't tell (fresh review I3; runner review I1)
privdrop judges `/companions` from inside the container only.
- **It detects:**
  - `/companions` being the same folder as `/data`, by inode;
  - an overlap of their container paths;
  - a folder holding anything but `gluetun`;
  - a folder that isn't root's, or that others can write.
- **It can't detect:** a host folder mounted at `/companions` that sits *inside* the host data folder, or inside another container's folder, on its own. From inside, the two mounts look unrelated.
  - In such a folder, a root-owned `gluetun` folder and file would still be written. Whoever owns the enclosing folder, such as the data folder's PUID or another container's user, could rename `gluetun` away. The template-time check would then stop trusting it, but gluetun mounts by host path.
- **Defences today:** the template-side check (`GluetunPaths::check`) refuses such host paths when the user types them. The guide says to keep the folder outside Lodestar's data folder and outside any other container's folder.
- **The real check,** comparing the mounts' sources in `/proc/self/mountinfo`, is a follow-up slice the runner is opening. It isn't in this PR.

### Natively (no privdrop; Lodestar runs as its user)
- **Only a folder you name.** There's no default beside the data folder (fresh review I1, I4). A default would name a folder on this machine in an Unraid template that may run on another, and could refuse a template for a field the user never filled in. This narrows the runner's "defaulting to a sibling" to an explicit folder; the runner gets it as a flag.
- **The checks, before anything is created:**
  - the folder's parent must resolve to itself;
  - the folder must not be, hold or sit in the data folder, comparing canonical paths (fresh review I2: a data folder reached through a link).
- **The write:** the folder and `gluetun` are opened `O_NOFOLLOW` and written through descriptors, as above.
- **Both folders must be Lodestar's user's (or root's), with no group or other write.** Otherwise it refuses with `companions_dir_shared`, and nothing is created in them.
- **An existing `post-rules.txt` that differs** is left as it is, and the template falls back to plan A. It is never overwritten.
- **A native install has no root step:** it writes the file as its own user, so there a compromised Lodestar could change it. `docs/operations.md` says so.
- **When it's written:** only once the template generates, so a refused template writes nothing.

## Owns added by the runner (2026-11-13, §2.33)
In addition to the brief's list:
- `crates/companions/tests/golden/**` and `packaging/companions/**`;
- `packaging/unraid/` (the new Path only);
- `TemplateRequest` in `crates/api`, plus the `openapi.json` and `schema.d.ts` regeneration;
- `docs/user/03-keep-lodestar-safe.md`, `08-add-companions.md` and `docs/operations.md` (the mount note);
- this file;
- `crates/lodestar/src/privdrop.rs` and its tests;
- `crates/companions/tests/flaresolverr.rs`: one line, the new field;
- `crates/lodestar/src/logging/events.rs` (two events), the log catalogue golden, `web/src/lib/features/settings/logging/event-names/system.ts`, and the event names in its `messages/{en,zh}.json`.

## Docs
- **Root-owned on purpose:** the guide (chapter 3), `docs/operations.md` §4.3 and the Unraid template's Path description say the `gluetun` folder and its file belong to root, and that Lodestar can only read them, on purpose. `docs/operations.md` gives the removal command for a host other than Unraid: `sudo rm -r <folder>/gluetun`.
- **Using the saved file now:** Unraid users can point gluetun's Post-rules path at the saved file today, after checking its SHA-256. Settings → Companions filling it in is **(planned)**.

## Checked
- **Local docker with the pinned gluetun v3.41.3** (§2.62). The test VMs were booked, so the runner allowed local docker. The setup used dummy WireGuard values on a TEST-NET endpoint, and the file was mounted read-only as a single-file bind, as the Unraid template does.
  - **The root write:** `save_companions_rules`, run as root, wrote the file root-owned with mode 0644 (2,202 bytes) in a root-owned 0755 `gluetun` folder. Its SHA-256 equals the guide's pin, and gluetun loaded all 3 rules in each family from it.
  - **The first build's writer, run the same way:**
    - **The file:** mode 0644, 2,202 bytes, and its SHA-256 equals the guide's pin.
    - **The rules:** gluetun loaded all 3 rules in each of IPv4 and IPv6.
    - **Inside the container:** the file was byte-equal and read-only, a write was refused, and `VPN_INTERFACE=wg0`.
    - **A replaced file** reached gluetun after `docker restart`.
    - **A missing file:** gluetun stopped with "is a directory", which fails closed.
- **No test VM run:** none is offered here until X-test-vm's windows end.
