# X-cmp13-installer-pins decisions

Installer pins in the companion catalogue: qBittorrent's official Windows installer gets a pin, so CMP-11's guided install can show a checked version and SHA-256 (owner cq4; `docs/decisions/2026-11-11-CMP-11.md`, "The format can't hold this pin yet"). It also carries CMP-13's two optional follow-ups (runner, 2026-11-11). These are the slice's decisions under §2.33. The pin-time source is a runner call (§2.26, §2.33), flagged to the owner. Nothing here loosens a run-time rule: Lodestar still never downloads or runs the installer.

## The catalogue format (`crates/companions/src/catalogue.rs`)
- **An artifact may name its own `release` folder, and only an installer may.** Every other kind still takes its pins from `native.release` on the entry's fetch `hosts`; a `release` on one is refused (`source`).
- **The folder's host must be in a new compiled-in list, `INSTALLER_SOURCES`** (`downloads.sourceforge.net`). It must also be outside both `DOWNLOAD_HOSTS` and the entry's `hosts`, so the two lists can't meet in either direction (`host_not_allowed`). A test checks the constants are disjoint.
  - Adding an installer source is a code change with its own review, like `DOWNLOAD_HOSTS`.
  - Nothing at run time reads `INSTALLER_SOURCES`: CMP-1's fetch takes the entry's `hosts`, and the manager refuses an installer before any fetch (`guided_only`, unchanged).
- **The folder's shape is exactly SourceForge's `/project/<project>/<folder>/`** (`source`), as strict as `native.release`'s `/<owner>/<project>/releases/download/`.
- **A pinned installer with its own folder** is checked against that folder and its host alone, never against the fetch hosts: `<release><version folder>/<file>`, an `.exe` or `.msi`, and the entry's one version, under the same rules as every other pin.
- **Old catalogues still load.** The field is optional (`serde(default)`). An installer without it is checked against `native.release` and the fetch hosts, exactly as before, so a SourceForge pin there is refused. That keeps the existing test catalogues in `tests/manager.rs`, `tests/windows_qbittorrent.rs` and `tests/fetch.rs`, which pin an installer from the GitHub folder, valid and unchanged. Today's `catalogue.json` is unchanged.
  - Requiring the folder on every pinned installer was considered. It isn't needed for safety, because Lodestar never fetches an installer, and it would have meant editing CMP-11b's tests outside this slice's paths.
- **`Artifact::release`** is public, `None` for everything but a pinned (or folder-naming) installer, so CMP-11's guide can name where the file came from if it wants to. CMP-11 still shows only `DOWNLOAD_PAGE`.

## The pin-time source (runner, 2026-11-11, approved as recommended)
- **What upstream publishes:**
  - qBittorrent's installers are official only on SourceForge and FossHub. Run time never fetches from either (runner, 2026-11-11).
  - SourceForge's `qbittorrent-win32/qbittorrent-<version>/` folder carries a detached OpenPGP signature beside each installer (`qbittorrent_5.3.0_x64_setup.exe.asc`, `qbittorrent_5.3.0_lt12_x64_setup.exe.asc`). The runner checked this from its side; this session's network couldn't reach SourceForge.
- **The choice:** the pin tool reads the installer and its `.asc` from SourceForge at pin time only, and pins it only if the signature verifies against qBittorrent's release key. **The signature is the trust anchor, not the host.** FossHub isn't used.
- **The key: `D8F3DA77AAC6741053599C136E4A2D025B7CC9A2`** (sledgehammer999, qBittorrent's release manager).
  - Source 1: the issuer fingerprint of the OpenPGP signature on qBittorrent's `release-5.3.0` tag on GitHub, read here with `git cat-file` and `gpg --list-packets` (an RSA key, SHA-256 signature).
  - The key file isn't committed in this PR: this session couldn't fetch it (github.com, `keys.openpgp.org` and `keyserver.ubuntu.com` all return 403 here). So the tool fetches it at pin time from `keyserver.ubuntu.com` and `github.com/sledgehammer999.gpg`, each of which must serve that primary key, and merges both copies so a revocation in either is kept. **That is availability, not a second source for the fingerprint** (self-review I2): a lookup by fingerprint can only return the key with that fingerprint. The fingerprint committed in `upstream.py` is the trust anchor.
  - Only that key is used, minimally exported. The report carries it, to be committed as `keys/D8F3….asc` in the first pin PR (runner, 2026-11-11).
  - **The fingerprint's sources, honestly:** both of today's are on GitHub (the tag signature's issuer, and sledgehammer999's key list). A source outside GitHub (Arch's `validpgpkeys`, Debian's `debian/upstream/signing-key.asc`, Fedora's spec) was asked of the runner; this session can't reach them. The first pin PR's reviewer confirms it against one.
  - Once committed, `keys/` wins and nothing is fetched.
- **Threat model:**
  - **A tampered SourceForge mirror, or a hostile redirect, serving other bytes:** they don't verify against the pinned key, so the pin is refused. Every hop goes through `check_url`: `https`, no port or user information, `downloads.sourceforge.net`, `sourceforge.net` or one DNS label before `.dl.sourceforge.net`, at most 5 hops.
  - **A mirror serving a different genuinely signed file** (an older installer, the `_lt12_` build, a source tarball) with its real `.asc`: **the signature alone doesn't catch this**, since it covers bytes, not a name or version (self-review B2). Binding the bytes to the version is pending the runner's decision; see "Open" below.
  - **A substituted key at a key source:** the tool uses only a key whose primary fingerprint is the pinned one, and a fingerprint can't be forged. Both sources must serve it, and a source that doesn't refuses the pin.
  - **A compromised release key, or a malicious upload signed by it:** not caught here. That's upstream's own trust, the same as every signed companion, and the pin is still the run-time gate: what's pinned is what you're told to check.
  - **SourceForge serving a different file under the same version:** `sha_changed` refuses a re-pin of one version as other bytes (CMP-13).
  - **A wrong fingerprint in the policy:** see the fingerprint's sources above.
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
- **The deadline holds during a read** (self-review B1, I1). Each read is one `recv` (`read1`), and before each the socket's timeout is cut to what's left of the 30 minutes, at most 60 s. A body that stalls or drips can't outlast the deadline (`timeout`); a socket already closed at the end of the body is ignored. Tested through `fetch()` against a real local server: complete length-delimited and chunked bodies, a stall and a drip. **Not covered:** a server that drips its response headers. That's bounded only by the 60 s socket timeout per recv and the workflow's 30-minute job limit.
- **A single-platform manifest's platform is checked.** Its config blob is fetched by digest and checked against that digest. Its `os/architecture` must be `linux/amd64`, as an index's list must contain it. The blob may be redirected to the registry's blob host:
  - `pkg-containers.githubusercontent.com` for ghcr.io and lscr.io;
  - `production.cloudflare.docker.com` for Docker Hub.

  The registry token isn't sent there.

## Open (runner)
- **B2, binding the installer's bytes to its version.** Proposed:
  - SourceForge's own folder feed, from `sourceforge.net` rather than a mirror, must state the downloaded size and MD5 for exactly that path;
  - the signature's creation time must not be before the GitHub `release-<v>` tag's date.
- Until the runner decides, the installer path stays as built, and the first pin PR mustn't include an installer pin.

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
    - a SourceForge pin without its folder (checked against the fetch hosts);
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
  - complete bodies, a stall and a drip, through `fetch()` on a real socket;
  - the loader's invariants and property tests, run over a catalogue with a pinned installer too;
  - a single-platform manifest's platform, another platform, and a config that isn't the one named.
