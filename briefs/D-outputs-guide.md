# D-outputs-guide: User guide: connect Prowlarr, Sonarr and Radarr; the bitmagnet export (docs)

**Why:** OUT-2 (#735, merged 8e40a635) shipped the Outputs screen. Users need a guide to connect their *arr apps safely, and to export to bitmagnet.

**Skills:** the project `user-docs` skill on top of `github-docs`. The audience is someone who can install from the Unraid app store but may not know how to set this up (owner §2.14). Define terms and recommend the safe choice. Use screenshots wherever you can, made from the real app and under the user-docs skill's screenshot rules.

**Facts come from the code and decisions files, not from memory:**
- TZ-0's decisions file:
  - the Torznab endpoint `/api/v1/torznab/api`;
  - the key and its scope: what it can do, what it can't, how to reveal and rotate it through the password dialog.
- OUT-0's and OUT-2's decisions files (`docs/decisions/2026-11-12-OUT-2.md`):
  - the bitmagnet export is a plain download, and its job is found on Jobs;
  - a capped export shows as truncated, because the file has no trailer, so tell the user what to do;
  - a cancelled or failed export means delete the file;
  - adult items and the DHT-lookup dialog.

**Must cover** (slices.yaml notes):
1. **Connecting the apps:** Prowlarr (recommended), and Sonarr and Radarr directly. Leave the apps' **API Path as `/api`**.
2. **The Sonarr port trap:** 5000 against 5045. State which port Lodestar serves on and what each number is.
3. **Sonarr's default categories miss 5030 and 5040** (TV with no parsed resolution). Tell the user to add them.
4. **What each app shows** for a non-200 Torznab error document (rv-715). Check this against the apps' own docs or source where you can reach them. Where you can't verify it, say less, never guess.
5. **The Torznab key:** keep it secret, rotate it if exposed, and treat it as different from an API key or a login.
6. **The bitmagnet export.**

## Owns
- `docs/user/outputs.md` (new)
- `docs/user/index.md`, `docs/user/glossary.md`, `docs/user/revisions.md` (the entries for this chapter)
- any screenshots under the folder the user-docs skill names

## Checks
- the user-docs and github-docs lint gates;
- links resolve;
- `npm run check` in `web/` if you touch in-app help (you shouldn't need to).

Branch `slice/D-outputs-guide`; PR title "D-outputs-guide: User guide: connect Prowlarr, Sonarr and Radarr; the bitmagnet export". Follow the steward skill. Message the runner "ready <pr> <full sha>" when CI is green.
