# lodestar-update: the update trust core

The pure verification core of Lodestar's updates (architecture §12.2): minisign signatures, the signed manifest, key statements and the key chain, and the trust state. It does no I/O and reads no clock; callers pass the bytes and the time, and write the trust state it returns.

**This file is the contract** that the release workflow (E-M6's `release.yml`) and the weekly re-sign must produce. Publishers keep `format: 1` until every supported version reads its successor.

## Signatures (minisign, Ed25519)

- **Public key:** the base64 line of 42 bytes: `Ed`, the 8-byte key number, the 32-byte Ed25519 key. The key ID shown (`02910813433E81E0`) is the key number read as a little-endian `u64`, in upper-case hex.
- **Signature file:** exactly four lines (LF or CRLF; an optional final line ending; nothing else), at most 1 KiB:
  1. `untrusted comment: …` (not signed);
  2. the base64 of 74 bytes: `ED`, the key number, the 64-byte signature;
  3. `trusted comment: …`;
  4. the base64 global signature (64 bytes) over the signature ‖ the trusted comment's text.

  Each comment is at most 512 bytes of printable UTF-8 (tab allowed). Base64 is strict: the standard alphabet, canonical padding, no whitespace.
- **Only prehashed `ED` signatures are accepted** (Ed25519 over the BLAKE2b-512 of the message: `minisign -S`, the default since minisign 0.10). Legacy `Ed` signatures (`minisign -S -l`) are refused.
- **A signature verifies** only when the key number matches, the signature over the prehash verifies and the global signature verifies. All three or nothing.

## Signed documents

Every signed document is carried as the **exact bytes that were signed**:

```json
{"body": "<base64 of the body file>", "minisig": "<the .minisig file's text>"}
```

Nothing in a body is used before its signature verifies under a key allowed for it. Then the body is parsed as strict JSON: UTF-8, no BOM, no unknown or duplicate fields, integers only (no fractions or exponents). Each body has a `type`, so one kind of document can never be read as another, and the release key's `SHA256SUMS.minisig` can't be taken as a manifest.

**The one read ahead:** when a manifest's or freshness statement's signature names a key the installation doesn't have yet (the key changed while it was offline), its `statements` array is read before the document verifies. Each statement is its own recovery-signed document and is verified before its own body is read; nothing else in the unverified body is used. The document then verifies under the key those statements lead to, and is parsed strictly.

Times are Unix seconds (UTC). Digests are SHA-256 as 64 lower-case hex digits.

### The envelope (unsigned): `<channel>.json`

One file per channel, so a check is one request. At most **512 KiB**.

```json
{"manifest": {"body": "…", "minisig": "…"}, "freshness": {"body": "…", "minisig": "…"} | null}
```

### Manifest body (`type: "lodestar-manifest"`), signed by the channel's key

At most **384 KiB**. Stable and Beta manifests are signed by the release key; Nightly manifests by the nightly key.

```json
{
  "type": "lodestar-manifest",
  "format": 1,
  "channel": "stable" | "beta" | "nightly",
  "timestamp": 1790000000,
  "expires": 1792592000,
  "statements": [{"body": "…", "minisig": "…", "cosig": "…"}],
  "releases": [{
    "version": "0.5.3",
    "date": 1789900000,
    "level": "fix" | "feature" | "major",
    "security": false,
    "manual_steps": false,
    "min_from": "0.5.0" | null,
    "withdrawn": false,
    "migrates": false,
    "reindex": false,
    "reindex_bytes": 0,
    "notes": "…",
    "notes_url": "https://…" | null,
    "artifacts": [{
      "target": "x86_64-unknown-linux-musl",
      "kind": "binary" | "msi" | "portable",
      "url": "https://github.com/…",
      "sha256": "…",
      "size": 12345678
    }]
  }]
}
```

- `format` other than 1 is refused; a higher one as "needs a newer Lodestar". `min_from` and `notes_url` may be null or left out.
- **Caps** (refused, never truncated): at most 64 statements, 64 releases and 8 artifacts per release; notes at most 16 KiB; versions strict semver without build metadata (`+…`), at most 64 bytes, unique; one artifact per target and kind.
- `url`: at most 512 bytes, `https` only, a lower-case host on the download allowlist (`github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`), no user, port, query or fragment. `notes_url` follows the same rules on any host.
- `size`: 1 byte to 256 MiB. `target`: one of `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`. A new target needs a new format, because older versions refuse unknown values.
- `notes` are signed text, but the UI renders them as plain text or sanitised Markdown.

### Key statement body (`type: "lodestar-key"`), signed by the recovery key

At most **2 KiB**.

```json
{"type": "lodestar-key", "seq": 1, "role": "release" | "nightly" | "freshness", "key": "<minisign public-key line>", "timestamp": 1790000000}
```

`minisig` is the recovery key's signature over the body file. `cosig`, the old key of that role's, is optional and never sufficient: only its form is checked. The owner makes one with the minisign CLI (`packaging/keys/README.md`).

### Freshness body (`type: "lodestar-freshness"`), signed by the freshness key

At most **64 KiB**.

```json
{"type": "lodestar-freshness", "channel": "stable", "manifest_sha256": "<SHA-256 of the manifest body>", "timestamp": 1790600000, "expires": 1793192000, "statements": []}
```

It extends the manifest's expiry only, and can't carry new content.

## The key chain

- **Adding a key is a one-time choice.** A nightly or freshness key is added either as generation 0 or by a statement, never both: a key that a statement already introduced can't later be embedded as generation 0 (it would be a reused key and every chain would fail).
- **Generation 0** is what the build embeds (`keys/`): the recovery key, the release key (Stable and Beta) and the freshness key (`FCA9AFC5C8D864FE`). Nightly has no key until the owner creates one; then its `.pub` is copied into `keys/` (a one-line change in `src/keys.rs`, the default) or a recovery-signed statement introduces the role. Until then Nightly verifies nothing (`no_nightly_key`), which is reported, never silent. A build without a freshness key refuses freshness statements (`no_freshness_key`).
- **A statement is accepted** only with a valid recovery signature, a `timestamp` at most a day ahead of the local clock, and a `seq` exactly one above the current sequence (one sequence for all roles). Accepting it revokes the previous key of that role only. A role with no key is introduced by its first statement. A key that has been used before, by any role (or the recovery key), or that shares a key number with one, is refused.
- **Contiguity and equivocation:** a document that carries statements carries **the whole chain, from `seq` 1, in ascending order**, because a publisher can't know any verifier's sequence (an old supervisor starts from its embedded statements alone). A list that doesn't start at 1, or skips, is refused (`statement_gap`). Statements at or below the current sequence must equal the accepted ones byte for byte; a different statement with an accepted `seq` refuses the document (`equivocation`, with a notice).
- **Every manifest published after a rotation carries the statements** that chain its key to the recovery key, and so does every freshness statement after a freshness rotation. **A manifest that carries a rotation is dated at or after the statement:** the rotation resets the mark to the statement's timestamp, so an earlier manifest is refused as older.
- **A chain holds at most 64 statements over its life** (one per rotation of any role), the same as a document's cap, so `trust.json` always fits its 256 KiB.
- **Accepted statements are stored normalised:** the body re-encoded, the recovery signature with a fixed untrusted comment, and no cosig, so whoever carried a statement can't pad what an installation saves.
- **The floor** is the newest valid chain among the embedded statements (`keys/statements.json`, empty now), `trust.json`'s, and those inside any verified overlay. Revoked keys are derived from it, together with those `trust.json` records.

## Trust state (`<data>/bin/trust.json`)

At most **256 KiB**, strict, written by the updater (temporary file, fsync, rename, directory fsync):

```json
{
  "format": 1,
  "seq": 1,
  "statements": [{"body": "…", "minisig": "…"}],
  "revoked": ["<minisign public-key line>"],
  "channels": {"stable": {"hwm_ts": 1790000000, "hwm_sha256": "…" | null, "fresh_ts": 0, "fresh_sha256": null}}
}
```

On load every statement is verified again in sequence and `seq` must agree with them; anything else is `trust_damaged`, which blocks installs.

- **`revoked` is additive.** Each build derives the revocations its own generation 0 implies and keeps the stored ones too (the union), so an older image or a rolled-back version with other generation-0 keys still loads the file and honours every revocation a newer build recorded. An extra entry can only refuse more.
- **Marks more than a day ahead of the clock mean the clock is behind.** Every mark is a verified timestamp accepted at most a day ahead, so the check refuses with `ahead_of_clock` (notice `clock_behind`) and leaves the file alone; the marks still hold once the clock is right. Only a file that fails to load is `trust_damaged`.

- **High-water mark per channel,** on the manifest's own signed timestamp. An older manifest is refused. The same timestamp is accepted only with the same body SHA-256 (a refetch); a different body at the mark is refused.
- **Freshness mark per channel,** with the freshness body's SHA-256, under the same rule: a freshness statement is accepted only if newer than the channel's mark, or the same one again.
- **Time limits:** a manifest, freshness statement or key statement dated more than **1 day** ahead of the local clock is refused. `expires` must be after `timestamp` and at most **30 days** after it. A manifest is **current** while it, or its accepted freshness statement, hasn't expired. One that isn't current blocks installs only.
- **Freshness acceptance:** the current, non-revoked freshness key signed it; its channel and `manifest_sha256` match the manifest; the manifest verified under the current key of its channel's role; it passes the freshness mark; and the manifest passed the high-water mark. A refused freshness statement leaves the manifest itself accepted.
- **Rotations:** a release or nightly rotation sets the high-water mark of the channels that key signs to the statement's timestamp (with no digest, so a manifest at exactly that time passes); a freshness rotation resets only the freshness marks, the same way.
- **Forward only:** `TrustState::merge(old, new)` needs `new`'s statements to extend `old`'s. With a higher sequence, `old`'s marks take only the resets of the statements `new` adds (so only the rotated role's marks can drop), then each mark is the later of the two; with the same sequence and statements, each mark is likewise the later of the two. On a tie a stored digest is kept. Statements compare by their signed bodies (a loaded file is normalised), so a file in another textual form still merges. Revocations are the union. Nothing else writes a state.
- **A missing `trust.json`** is rebuilt from the floor (`TrustState::rebuild`), each channel's high-water mark at its newest verified overlay manifest.

## Entry points

| Function | Checks |
|---|---|
| `check(envelope, channel, now, &Embedded, &TrustState)` | Everything above. Returns the manifest (with its exact body and signature, for the overlay), whether it's current, the freshness outcome, the next trust state and the notices (`clock_behind` comes back as the `ahead_of_clock` refusal's notice; `clock_ahead` when the newest manifest isn't current). |
| `install(&Checked, version, target, kind)` | A current manifest, a release that isn't withdrawn, the artifact for the target and kind. Returns its URL, SHA-256 and size. |
| `overlay(manifest, minisig, binary_sha256, binary_len, version, target, &Embedded)` | From the built-in recovery key alone: the embedded statements, then the overlay manifest's own, the signature, then the binary's SHA-256 and size. No clock, no expiry, no high-water mark. A key the embedded statements revoke is refused; one revoked only later starts (`revoked_later`). |

Every refusal has a stable code (`Error::code`). No input panics, and nothing allocates beyond the caps.

## Tests

- `src/tests/vectors.rs`: golden envelopes in `tests/vectors/check/`, signed with fixed TEST ONLY keys (`src/testing.rs`). `LODESTAR_BLESS=1 cargo test -p lodestar-update vectors` regenerates them.
- `tests/vectors/minisign/`: a key, message and signatures made with the real minisign 0.11 tool (prehashed and legacy).
- Property tests in `src/tests/props.rs`; the `update` fuzz target in `fuzz/`.
- The `testing` feature exposes the test signer to the fuzz target and other crates' tests. No shipped build enables it.
