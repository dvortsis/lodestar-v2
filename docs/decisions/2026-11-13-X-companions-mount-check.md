# X-companions-mount-check decisions

This closes the gap that #825 recorded (`2026-11-13-X-gluetun-templates-hostfile.md`, "What the container side can and can't tell"). It follows the runner's brief `briefs/X-companions-mount-check.md` and is a runner §2.19 flag to the owner.

## The mount comparison
- **Where:** `post_rules::mounts_apart` in `crates/lodestar/src/companions.rs`. Two places call it:
  - privdrop, at start-up as root, before it writes anything;
  - the template read in a container, before it trusts the file.

  The second call means a file that an older build wrote into a nested folder isn't trusted after an upgrade either.
- **What it reads:** `/proc/self/mountinfo`. Each line gives a mount's device, the folder of that filesystem it shows (`root`), its mount point and its type.
- **What it refuses**, with kind `nested`:
  - **The data folder:** `/companions`'s own mount is on the same files as the data folder's mount, and the two roots are equal or nested either way. The data folder may be a subfolder of a bigger mount, so its root is the mount's root plus the rest of its path.
  - **Any other folder mounted in this container:** a companion's downloads, or a whole disk. The same rule applies, so a host folder inside another container's folder is refused when that folder is mounted here too.
  - **Lodestar's or another catalogue companion's `appdata/<name>`** anywhere in `/companions`'s root. This covers a companion folder that isn't mounted here. The names are the catalogue's container names, plus `lodestar` and `lodestar-worker`.
- **"The same files":** one of these holds:
  - the same device;
  - two ZFS datasets of one pool;
  - either mount is an Unraid user share (`fuse.shfs`).

  **Why the share rule:** a user share shows every pool's and disk's files under the same paths, so `/mnt/user/appdata/x` and a pool's `appdata/x` report different devices.

  **The cost:** with a share, any other real mount whose root holds `/companions`'s root counts as nested, for example a whole disk mounted at `/` in the container. That refusal is deliberate: failing closed is the safe side.
- **ZFS** (Unraid 6.12 and later makes one dataset per share). Each dataset has its own device, and mountinfo shows its root relative to the dataset. So the dataset's path in its pool (from the source field: `cache/appdata` gives `appdata`) goes in front of the root, and roots compare as paths in the pool, as a user share shows them. This catches:
  - a folder in another companion's appdata on an appdata dataset;
  - a child dataset inside the data folder;
  - a data folder mapped by its pool path, with the companion-files folder mapped by the share (fresh review).
- **Mounts:**
  - of two at one point, the one listed last (on top) counts;
  - anything mounted *inside* `/companions` refuses (`nested`);
  - a root ending in `//deleted` (an unlinked bind source, such as a replaced `resolv.conf`) is read without the suffix;
  - names that aren't UTF-8 are read with replacement characters. Two different names can only read alike, which refuses, never the reverse.
- **What it ignores:** pseudo filesystems (proc, sysfs, tmpfs, devpts, mqueue, cgroup, overlay and the rest), and the container's own `/`. Docker's `/etc/hosts`, `/etc/hostname` and `/etc/resolv.conf` are files deep in Docker's own folder, so they never nest with a companion-files folder.
- **Parsing is strict.** A table refuses as a whole, with kind `mounts` and nothing written, when:
  - it holds no lines, more than 4,096 lines, or a line over 8,192 bytes;
  - a line has fewer than ten fields or no `-` separator;
  - a device isn't `major:minor`;
  - a backslash escape isn't three octal digits;
  - a path isn't absolute and clean (no empty, `.` or `..` component).

  The same kind covers a `/companions` that isn't a mount of its own, or is a pseudo one, and a data folder whose mount isn't listed. It never assumes safe.
- **The log:** both kinds go to the existing `system.companions_dir_left` event, which records the kind only. No new event, code or message was added. The guide tells the user to read that line.

## What's still not detected
- **A host path reached through a symlink on the host.** Docker resolves it before mounting, and mountinfo shows the resolved root, so the comparison uses the real folder. A link that points between devices that aren't a user share is compared by device only, and would pass if the devices differ.
- **Two different filesystems showing the same files in other ways**, for example a bind-mounted host folder behind a second FUSE layer, or an NFS export of the data folder. Only Unraid's `fuse.shfs` and ZFS pools are special-cased. A ZFS dataset whose mount point doesn't follow its dataset path is compared by its dataset path, not where it is mounted.
- **A companion's folder that is neither mounted here nor under `appdata/<catalogue name>`**, such as a qBittorrent config in `/srv/qb`. The template side already refuses the routed companion's own folders by host path (`GluetunPaths::check`), and the guide says to keep the folder apart.

In each of these cases the root-owned file and folders still stop the dropped Lodestar from changing the rules. What remains is someone who can write the enclosing host folder.

## Native
- **The parent folder is opened with `openat2(RESOLVE_NO_SYMLINKS)`** where the kernel and any seccomp filter allow it (ENOSYS and EPERM fall back), so no link is followed anywhere on the way. The companion-files folder is then created (0755, then `fchmod`) and opened relative to that descriptor.
- **Where `openat2` is missing** (ENOSYS, or not Linux), the existing checks stand alone: the parent must resolve to itself, there's a canonical comparison with the data folder, and `O_NOFOLLOW` opens. The link refusals are unchanged.

## Tests
- **The container template read** now takes its side injected: where `/companions` is, the mount table, and the owner. The #825 test that asserted only when `/companions` happened to be missing is now unconditional.
- **mountinfo fixtures** cover nested either way, equal, sibling, a different device, a user share against a pool, a bind of a subfolder, other mounts, escaped spaces, malformed input, an over-long line, too many lines, and `/companions` not being a mount.
- **privdrop's tests** pass a fixture in which the folder is a mount of its own.

## Checked (§2.62)
Docker Hub was rate-limiting pulls (429), so this used a local mount namespace instead: `unshare -m` with `mount --bind`, the same kind of bind mounts Docker makes. The kernel's own `/proc/self/mountinfo` from each setup went through `mounts_apart`:
- **Sibling folders** bound at the data and companion-files points: `Ok`.
- **The companion-files folder inside the data folder:** `nested`.
- **The companion-files folder inside another bound folder** whose name holds a space (escaped `\040` in the table): `nested`.

No test VM run.
