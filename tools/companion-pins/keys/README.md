# Pinned upstream keys

One public key per file, named by its full primary-key fingerprint: `<FINGERPRINT>.asc` (40 or 64 upper-case hex digits). `upstream.py` names the fingerprints each artifact may be signed by.

The pin tool imports a file only if it holds exactly that one primary key, into a throwaway `GNUPGHOME`, and refuses a signature by any other key. No key is pinned yet: no artifact the catalogue can hold today is signed upstream (`docs/decisions/2026-11-11-CMP-13.md`).

Adding a key is a reviewed change. The PR says where the fingerprint came from, with at least two independent sources (the project's own site and its release announcements, for example), never only the key server that served the key.

Never commit a private key. The tests make their own keys in a temporary home.
