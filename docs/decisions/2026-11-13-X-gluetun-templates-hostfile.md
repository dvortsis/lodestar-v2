# X-gluetun-templates-hostfile decisions

Lodestar now saves gluetun's post-rules file itself, for an Unraid template with gluetun, and the template mounts that file. This follows the runner's brief `briefs/X-gluetun-templates-hostfile.md` and the runner's answers of 2026-11-13. These defaults are a runner §2.19 flag to the owner. The guide's step 1 (the user saving and checking the file) stays as the fallback, and was plan A in X-gluetun-templates (`2026-11-12-X-gluetun-leak.md`).

## Where the file lives
- **A companion-files folder of its own, never Lodestar's data folder** (runner). Mounting a file from Lodestar's appdata into gluetun would be an exception to the rule that companions never mount Lodestar's own folder, because it holds Lodestar's secrets (§18.10, `packaging/companions/README.md`). Weakening that rule is the owner's call, so the file goes somewhere else.
  - **Unraid:** Lodestar's templates (`packaging/unraid/lodestar.xml`, `lodestar-gluetun.xml`) gain an optional **Companion files** Path. The host folder defaults to `/mnt/user/appdata/lodestar-companions`, mounted read-write at `/companions`. It holds one small file, so `/mnt/user` is fine there.
  - **Natively:** `lodestar-companions`, beside the data folder, created if it's missing. A native data folder whose sibling the rules refuse (for example one under `~/.local`, a dot folder) falls back to the user saving the file.
  - **The file:** `<companion files>/gluetun/post-rules.txt`. gluetun's template mounts it read-only as a single-file bind.
- **`ServiceInput.gluetun_paths`** (`GluetunPaths { companions }`) carries the host folder.
  - **Unraid templates:** only the gluetun template reads it. With it, the **Post-rules** Path points at the saved file and the Overview says Lodestar saved it. Without it, the Path keeps plan A's default and the save-it-yourself note.
  - **Compose:** carries the rules inline either way, so it ignores the field.
  - **The CLI** generates compose only, so it sets none.
- **The folder must hold nothing of Lodestar's or another companion's.** `GluetunPaths::check` refuses a folder that overlaps the running data folder where it's known, or that sits in Unraid's `appdata/lodestar` or `appdata/lodestar-worker` (which a container can't see). It also refuses one that overlaps another companion's known folder, or that has a dot component.
- **The API field:** `TemplateRequest.companions_dir` (the host folder). It's read only for an Unraid template that includes gluetun.
  - **In a container,** both the field and the `/companions` mount are needed. Without either, the template falls back to plan A, whose Overview tells the user to save the file.
  - **The UI field** is the follow-up slice X-gluetun-hostfile-ui (runner). Until it lands, the guide marks Lodestar saving the file as **(planned)**.

## How the file is written
- **Atomically:** a temporary file created new in the same folder, fsync, rename, then a best-effort fsync of the folder. A rename doesn't follow a link at the destination, so a link there is replaced rather than written through, and a folder there is refused.
- **Under the companion-files folder only:** below it every name is fixed. The companion-files folder and its `gluetun` folder must be real folders, not links.
  - **Natively,** the folder must also resolve to itself (`canonicalize`), so a link anywhere on the way can't lead into the data folder.
- **Mode 0644:** gluetun reads it as root, and nothing in it is secret.
- **Written only once the template generates,** so a refused template writes nothing.
- **A replaced file reaches gluetun on its next start,** which is when gluetun reads its post-rules. A single-file bind keeps the old file until then, as checked below.

## Handing `/companions` to PUID:PGID (`privdrop.rs`, runner-approved)
On Unraid, Docker creates a missing host folder as root. Lodestar drops to PUID before it writes, so privdrop now hands the folder over at start-up, within these limits:
- **Only `/companions` and `/companions/gluetun`,** with no recursion and nothing else. Each is opened `O_NOFOLLOW | O_DIRECTORY` and changed through its descriptor with `fchown`.
- **Only a folder that qualifies:**
  - it is a real folder;
  - it isn't the filesystem root;
  - it isn't the data folder by identity (`st_dev`/`st_ino`);
  - its path doesn't overlap the data folder's;
  - it holds nothing but `gluetun`, which holds nothing but `post-rules.txt` and its temporary files.
- **Why the last rule:** a mount of a whole share (such as `appdata`), or of a folder holding the data folder, is never handed over. That would let Lodestar rename other apps' folders.
- **Otherwise it is left as it is** and one Info line says why (`system.companions_dir_left`, with a kind only). This includes no mount at all, the usual case. It never stops start-up and never creates `/companions`. The dropped user creates `gluetun` when it writes.
- **A limit:** from inside a container, a host folder *inside* the data folder, mounted here on its own, can't be told apart. The dropped user owns the data folder's tree already, so handing it over gives nothing new. The template side refuses such host paths.

## Owns added by the runner (2026-11-13, §2.33)
These are in addition to the brief's list:
- `crates/companions/tests/golden/**` and `packaging/companions/**`;
- `packaging/unraid/` (the new Path only);
- `TemplateRequest` in `crates/api`, plus the `openapi.json` and `schema.d.ts` regeneration;
- `docs/user/03-keep-lodestar-safe.md` and `08-add-companions.md`;
- this file;
- `crates/lodestar/src/privdrop.rs` and its tests;
- `crates/companions/tests/flaresolverr.rs`: one line, the new field;
- for privdrop's one log line, two events in `crates/lodestar/src/logging/events.rs`, the log catalogue golden, `web/src/lib/features/settings/logging/event-names/system.ts` and the event names in its `messages/{en,zh}.json`.

## Checked
- **Local docker, the pinned gluetun v3.41.3** (§2.62: the test VMs were booked, so the runner allowed local docker). The setup used dummy WireGuard values on a TEST-NET endpoint. The file came from Lodestar's own save path and was mounted read-only as a single-file bind, as the Unraid template does:
  - **The file:** mode 0644, 2,202 bytes, and its SHA-256 equals the guide's pin.
  - **The rules:** gluetun loaded all 3 rules in each of IPv4 and IPv6.
  - **Inside the container:** the file was byte-equal and read-only, a write was refused, and `VPN_INTERFACE=wg0`.
  - **A replaced file** reached gluetun after `docker restart`.
  - **A missing file:** gluetun stopped with "is a directory", which fails closed.
- **No test VM run:** none is offered here until X-test-vm's windows end.
