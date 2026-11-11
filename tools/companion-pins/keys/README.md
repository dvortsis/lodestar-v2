# Pinned upstream keys

One public key per file, named by its full primary-key fingerprint: `<FINGERPRINT>.asc` (40 or 64 upper-case hex digits). `upstream.py` names the fingerprints each artifact may be signed by.

The pin tool imports a file only if it holds exactly that one primary key, into a throwaway `GNUPGHOME`, and refuses a signature by any other key. Pinned so far:
- **`D8F3DA77AAC6741053599C136E4A2D025B7CC9A2`**, qBittorrent's release key (sledgehammer999). It signs the Windows installers on SourceForge and the release tags on GitHub.
  - Its fingerprint's sources: the issuer of the signature on GitHub's `release-5.3.0` tag, and sledgehammer999's key list on GitHub. Both are on GitHub; a source outside it (a distribution's packaging) is still to be confirmed. A key server's answer to a lookup by fingerprint confirms nothing, so it isn't a source.
  - The file isn't committed yet. Until it is, a pin run fetches the key from `keyserver.ubuntu.com` and `github.com/sledgehammer999.gpg`, uses it only if both serve that primary key, and prints it in the report for the first pin PR to commit here (`docs/decisions/2026-11-12-X-cmp13-installer-pins.md`).

Adding a key is a reviewed change. The PR says where the fingerprint came from, with at least two independent sources (the project's own site and its release announcements, for example), never only the key server that served the key.

Never commit a private key. The tests make their own keys in a temporary home.
