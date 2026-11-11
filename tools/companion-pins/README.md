# companion-pins

CMP-13's pin-refresh tool. It makes the pins in `crates/companions/data/catalogue.json`: each artifact's URL, size and SHA-256, what was checked of upstream's own signature, and each container image's digest. Every pin in the catalogue comes from this tool (plan `docs/plan/2026-11-06-v06-companions.md` §4, CMP-G).

Standard library Python (3.12 or later) and the system `gpg`. Decisions, the upstream signing story and what the tool refuses: `docs/decisions/2026-11-11-CMP-13.md`.

## Files
- `pin.py`: the tool.
- `upstream.py`: the trust policy. File names per release, which artifacts upstream signs and with which keys (by fingerprint), image tags, and the pin-time checks.
- `keys/`: the pinned public keys, `<FINGERPRINT>.asc`.
- `test_pin.py`: offline tests: `python3 -m unittest discover -s tools/companion-pins -p 'test_*.py' -v`.

## Making a pin
Run the **Companion pins** workflow (Actions, then Run workflow) with the companion and version. It needs nothing but read access. It uploads `catalogue.json`, `report.md` and `catalogue.diff` as the `companion-pins` artifact and shows the report in the job summary. It never commits or opens a PR: you open the pin PR with that `catalogue.json` and paste the report into its description, and the runner reviews it.

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
