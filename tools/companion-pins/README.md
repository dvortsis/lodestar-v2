# companion-pins

CMP-13's pin-refresh tool. It makes the pins in `crates/companions/data/catalogue.json`: each artifact's URL, size and SHA-256, what was checked of upstream's own signature, and each container image's digest. Every pin in the catalogue comes from this tool (plan `docs/plan/2026-11-06-v06-companions.md` §4, CMP-G).

Standard library Python (3.12 or later) and the system `gpg`. Decisions, the upstream signing story and what the tool refuses: `docs/decisions/2026-11-11-CMP-13.md`.

## Files
- `pin.py`: the tool.
- `upstream.py`: the trust policy. File names per release, which artifacts upstream signs and with which keys (by fingerprint), image tags, and the pin-time checks.
- `keys/`: the pinned public keys, `<FINGERPRINT>.asc`.
- `test_pin.py`: offline tests: `python3 -m unittest discover -s tools/companion-pins -p 'test_*.py' -v`.

## Making a pin
Run the **Companion pins** workflow from `main` (Actions, then Run workflow) with the companion and version. A run on another branch is refused, but only as a guard against mistakes: such a run uses that branch's own workflow and tool. It needs nothing but read access. It uploads `catalogue.json`, `report.md` and `catalogue.diff` as the `companion-pins` artifact and shows the report in the job summary. It never commits or opens a PR: you open the pin PR with that `catalogue.json` and paste the report into its description, and the runner reviews it.

### Reviewing a pin PR
The report's provenance is printed by the run itself, so check the run, not only the report:
1. `gh run view <run id> --json headBranch,headSha,event`: `headBranch` must be `main` and `event` must be `workflow_dispatch`.
2. `git diff <headSha> origin/main -- tools/companion-pins` must be empty: the tool and its trust policy are `main`'s.
3. The report's `pin.py` and `upstream.py` SHA-256 match those files on `main`.
4. The diff touches only pins and digests, and the report's dispatcher `Proxy` lines are still behind the own-proxy check (Prowlarr).

Locally, the same run:

```sh
python3 tools/companion-pins/pin.py --catalogue crates/companions/data/catalogue.json \
  --out /tmp/catalogue.json --report /tmp/report.md --companion prowlarr --version 1.28.2.4885
```

- `--tag`: the release tag, when the tool can't derive it (qBittorrent's userdocs builds: `release-<qBittorrent>_v<libtorrent>`).
- `--image-tag`: the container tag whose digest is pinned, when not the default.
- `--artifacts linux-x86_64,linux-aarch64`: pin only these (every pin in an entry must still name one version).
- `--skip-image`: leave the container digest as it is.

On any refusal it exits 1, says why on stderr and writes nothing.

### qBittorrent's Windows installer
It's pinned with the Linux builds, at the same version, from SourceForge (read only at pin time, never by Lodestar) and only with the release key's signature on it. Until `keys/D8F3DA77AAC6741053599C136E4A2D025B7CC9A2.asc` is committed, the run fetches that key (checked by fingerprint after import) and prints it in the report. Commit it in the pin PR. The installer is also tied to its version: the final download URL, SourceForge's own feed (size and MD5 for that path) and the GitHub release tag's date must all agree.
