# Pinned upstream keys

One public key per file, named by its full primary-key fingerprint: `<FINGERPRINT>.asc` (40 or 64 upper-case hex digits). `upstream.py` names the fingerprints each artifact may be signed by.

The pin tool imports a file only if it holds exactly that one primary key, into a throwaway `GNUPGHOME`, and refuses a signature by any other key. Pinned so far:
- **`D8F3DA77AAC6741053599C136E4A2D025B7CC9A2`**, qBittorrent's release key (sledgehammer999). It signs the Windows installers on SourceForge and the release tags on GitHub.
  - The fingerprint is confirmed by two GitHub sources: the release tag's signature issuer and sledgehammer999's published keys. Both rely on GitHub. Neither relies on SourceForge, which is the threat this check covers. No source outside GitHub was reachable. A pin PR's reviewer re-checks it from outside GitHub where they can.
  - The file isn't committed yet. Until it is, a pin run fetches the key from `keyserver.ubuntu.com` and `github.com/sledgehammer999.gpg` (fetching only: each copy is checked by fingerprint after import) and prints it in the report for the first pin PR to commit here (`docs/decisions/2026-11-12-X-cmp13-installer-pins.md`).

Adding a key is a reviewed change. The PR says where the fingerprint came from, with at least two independent sources (the project's own site and its release announcements, for example), never only the key server that served the key.

Never commit a private key. The tests make their own keys in a temporary home.
