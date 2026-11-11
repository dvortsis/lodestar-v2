# X-cmp13-installer-pins decisions

Installer pins in the companion catalogue: qBittorrent's official Windows installer gets a pin, so CMP-11's guided install can show a checked version and SHA-256 (owner cq4; `docs/decisions/2026-11-11-CMP-11.md`, "The format can't hold this pin yet"). It also carries CMP-13's two optional follow-ups (runner, 2026-11-11). These are the slice's decisions under §2.33. The pin-time source is a runner call (§2.26, §2.33), flagged to the owner. Nothing here loosens a run-time rule: Lodestar still never downloads or runs the installer.

## The catalogue format (`crates/companions/src/catalogue.rs`)
- **An artifact may name its own `release` folder, and only an installer may.** Every other kind still takes its pins from `native.release` on the entry's fetch `hosts`; a `release` on one is refused (`source`).
- **The folder's host must be in a new compiled-in list, `INSTALLER_SOURCES`** (`downloads.sourceforge.net`). It must also be outside both `DOWNLOAD_HOSTS` and the entry's `hosts`, so the two lists can't meet in either direction (`host_not_allowed`). A test checks the constants are disjoint.
  - Adding an installer source is a code change with its own review, like `DOWNLOAD_HOSTS`.
  - Nothing at run time reads `INSTALLER_SOURCES`: CMP-1's fetch takes the entry's `hosts`, and the manager refuses an installer before any fetch (`guided_only`, unchanged).
- **The folder's shape is exactly SourceForge's `/project/<project>/<folder>/`** (`source`), as strict as `native.release`'s `/<owner>/<project>/releases/download/`.
- **A pinned installer must name its folder** (`source`). Its pin is checked against that folder and its host alone, never against the fetch hosts: `<release><version folder>/<file>`, an `.exe` or `.msi`, and the entry's one version, under the same rules as every other pin.
- **Old catalogues still load.** The field is optional (`serde(default)`), and an unpinned installer needs no folder. Today's `catalogue.json` is unchanged.
- **`Artifact::release`** is public, `None` for everything but a pinned (or folder-naming) installer, so CMP-11's guide can name where the file came from if it wants to. CMP-11 still shows only `DOWNLOAD_PAGE`.

## The pin-time source (runner, 2026-11-11, approved as recommended)
- **What upstream publishes:**
  - qBittorrent's installers are official only on SourceForge and FossHub. Run time never fetches from either (runner, 2026-11-11).
  - SourceForge's `qbittorrent-win32/qbittorrent-<version>/` folder carries a detached OpenPGP signature beside each installer (`qbittorrent_5.3.0_x64_setup.exe.asc`, `qbittorrent_5.3.0_lt12_x64_setup.exe.asc`). The runner checked this from its side; this session's network couldn't reach SourceForge.
- **The choice:** the pin tool reads the installer and its `.asc` from SourceForge at pin time only, and pins it only if the signature verifies against qBittorrent's release key. **The signature is the trust anchor, not the host.** FossHub isn't used.
- **The key: `D8F3DA77AAC6741053599C136E4A2D025B7CC9A2`** (sledgehammer999, qBittorrent's release manager).
  - Source 1: the issuer fingerprint of the OpenPGP signature on qBittorrent's `release-5.3.0` tag on GitHub, read here with `git cat-file` and `gpg --list-packets` (an RSA key, SHA-256 signature).
  - Source 2: the pin run itself. The key file isn't committed in this PR: this session couldn't fetch the key (github.com, `keys.openpgp.org` and `keyserver.ubuntu.com` all return 403 here). So the tool fetches it at pin time from two places, `keyserver.ubuntu.com` and `github.com/sledgehammer999.gpg`, and each must serve that primary key.
  - Only that key is used, minimally exported. The report carries it, to be committed as `keys/D8F3….asc` in the first pin PR. That run's report is the cross-check evidence (runner, 2026-11-11).
  - Once committed, `keys/` wins and nothing is fetched.
- **Threat model:**
  - **A tampered SourceForge mirror, or a hostile redirect:** the bytes don't verify against the pinned key, so the pin is refused. SourceForge's mirrors (`<mirror>.dl.sourceforge.net`) are allowed as redirect hops for exactly that reason. Every hop still goes through `check_url`: `https`, no port or user information, one DNS label before `.dl.sourceforge.net`, at most 5 hops.
  - **A substituted key at a key source:** the tool uses only a key whose primary fingerprint is the pinned one, and a fingerprint can't be forged. Both sources must serve it, and a source that doesn't refuses the pin.
  - **A compromised release key, or a malicious upload signed by it:** not caught here. That's upstream's own trust, the same as every signed companion, and the pin is still the run-time gate: what's pinned is what you're told to check.
  - **SourceForge serving a different file under the same version:** `sha_changed` refuses a re-pin of one version as other bytes (CMP-13).
  - **A wrong fingerprint in the policy:** two independent sources name it, the GitHub tag signature and the key servers' answer to a by-fingerprint lookup. The pin PR's reviewer checks the report's key against the tags.
  - **Not considered sufficient, so not built:**
    - two mirrors agreeing (SourceForge and FossHub, both uploads by the same maintainer);
    - a hash on a web page (qbittorrent.org publishes none).
    - Either would need the owner (a supply-chain loosening).

## The pin tool (`tools/companion-pins`)
- **An installer is pinned only with an OpenPGP policy:** an installer with an `Unsigned` policy is refused (`installer`). The recipe names its folder (`installer_release`) and the version's sub-folder (`installer_dir`, `qbittorrent-{version}`).
  - The tool's copy of `INSTALLER_SOURCES` is checked against the Rust one (drift test), and the folder may not be one of the entry's hosts.
- **Pin-time hosts are their own kind** (`PinTimeHosts`): exact names plus one mirror label. Every other fetch still refuses SourceForge and FossHub outright (`NEVER_HOSTS`, CMP-13). A test shows `downloads.sourceforge.net` is still refused on a normal fetch even when it's listed.
- **The libtorrent 2.0 build** (`qbittorrent_<v>_x64_setup.exe`) is the one pinned. SourceForge also carries an `_lt12_` build, which isn't pinned.
- **The catalogue gets `release` written before `pin`,** in the committed layout, and the entry's `hosts` are untouched.
- **One version per entry, as before.** The installer and the userdocs Linux builds must name the same qBittorrent version, so pin them together, or pin the installer alone when the userdocs version matches.
- **Fails closed, writing nothing:**
  - no `.asc` (`signature_missing`);
  - a bad signature, or one by another key (`bad_signature`);
  - a key file or key source with the wrong key (`unknown_key`);
  - fewer than two key sources with no committed key (`key_not_pinned`);
  - a redirect to any other host (`host_not_allowed`);
  - an unsigned installer policy (`installer`).

## CMP-13's follow-ups
- **The deadline holds during a read.** Before each read, the socket's timeout is cut to what's left of the 30 minutes, at most 60 s. A body that stalls or drips can't outlast the deadline, and a stall is refused as `timeout`. Tested against a real local server that stalls.
- **A single-platform manifest's platform is checked.** Its config blob is fetched by digest and checked against that digest. Its `os/architecture` must be `linux/amd64`, as an index's list must contain it. The blob may be redirected to the registry's blob host:
  - `pkg-containers.githubusercontent.com` for ghcr.io and lscr.io;
  - `production.cloudflare.docker.com` for Docker Hub.

  The registry token isn't sent there.

## Not changed
- `crates/companions/README.md`'s `native.release` row doesn't yet mention the installer folder. It's outside this slice's paths; the runner is told.
- CMP-11's guide shows the official download page, as before. Showing `Artifact::release` is CMP-11's choice to make.
- The CMP-13 decisions file still says the installer is refused; it's the record of that slice.

## Tests
- **Loader** (`tests/catalogue.rs`):
  - a pinned installer read from its own folder;
  - an unpinned installer with no folder, and the builtin catalogue with none;
  - the source lists disjoint;
  - refusals:
    - a pinned installer without a folder;
    - a folder on a non-installer artifact;
    - `http`, malformed, or a number;
    - an unknown host;
    - `sourceforge.net` without `downloads`;
    - a fetch host as the folder;
    - the installer source added to the entry's hosts;
    - every wrong folder shape;
    - a pin outside its folder, on a fetch host, in another project, or with no version folder;
    - a zip as an installer;
    - another version.
- **Pin tool** (fakes, fixture keys made with gpg):
  - the installer pinned through a mirror redirect, in the committed key order;
  - no `.asc`, a tampered installer, a signature by another key, a non-signature;
  - an unsigned policy;
  - redirects to an unknown host, two labels, a look-alike suffix, `http`, GitHub or FossHub, and a signature redirected away;
  - the key fetched from two sources (one holding an extra key), a source without it, and a single source;
  - the mirror-host rule;
  - the stalled body;
  - a single-platform manifest's platform, another platform, and a config that isn't the one named.
