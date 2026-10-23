# Release signing keys

Lodestar's updates are signed with [minisign](https://jedisct1.github.io/minisign/) (Ed25519). These are the **public** keys. The binary embeds them, and every update is checked against them before it runs (architecture §12.2). The private keys are never in this repository.

| File | Key ID | Role | Private key kept |
|---|---|---|---|
| `lodestar-release.pub` | `02910813433E81E0` | Signs Stable and Beta manifests | In the GitHub Actions environment `release` as `RELEASE_SIGNING_KEY`, which only `v*` tags can use, with the owner as required reviewer; plus the owner's offline backup |
| `lodestar-nightly.pub` | (to be generated) | Signs Nightly manifests; trusted only on the Nightly channel | In the environment `nightly` (main only) |
| `lodestar-freshness.pub` | (to be generated) | Signs only freshness statements (channel, manifest hash, timestamp, expiry) for manifests already signed; never new content | In the environment `freshness`, used only by the scheduled weekly re-sign on main |
| `lodestar-recovery.pub` | `67922DDF005FD310` | Signs **key changes** only, never releases | Offline only, password-protected, with the owner |

## Verifying a release by hand

```
minisign -V -p packaging/keys/lodestar-release.pub -m release.json
```

## Replacing a key

A key change is a signed statement. It names the key's **role** (release, nightly or freshness) and the new public key, and carries a **sequence number one higher than the last one published**. Installed copies accept it only under these rules (architecture §12.2):

- **The recovery key must sign it.** The current release key may co-sign, but its signature alone is never accepted. A leaked release key therefore can't rotate to an attacker's key.
- **Accepting it revokes the previous key of that role only,** for new installs.
- **A statement whose sequence number isn't higher than the last one accepted is refused,** so an old statement can't be replayed.
- **Installed copies keep this trust state** (the accepted statements, the sequence number, the revoked keys, and the newest manifest time seen) in `<data>/bin/trust.json`. It sits outside the database and every snapshot, so rolling back a failed update never undoes a revocation. Each Lodestar build also embeds the key statements published before it was built, so deleting `trust.json` can't undo a revocation that build knows about.

To rotate (the formats are in `crates/update/README.md`):
1. Generate a new key pair (`minisign -G -W`). Keep the new `.pub` file's second line, the base64 key line.
2. Write the key-change statement as a file, for example `statement.json`. It is one JSON object with exactly these fields:

   ```json
   {"type": "lodestar-key", "seq": 1, "role": "release", "key": "RWQ…the new key line…", "timestamp": 1790000000}
   ```

   - `seq` is one higher than the last statement published (the first is 1; one sequence for all roles).
   - `role` is `release`, `nightly` or `freshness`.
   - `timestamp` is the current time in Unix seconds (`date +%s`). Installed copies refuse a statement dated more than a day ahead of their clock.
   - The key must never have been used before, by any role.
3. Sign the file with the **recovery** key, with a prehashed signature (the default; never `-l`):

   ```sh
   minisign -S -s lodestar-recovery.key -m statement.json -t 'lodestar key statement'
   ```

   This writes `statement.json.minisig`. Check it with `minisign -V -p lodestar-recovery.pub -m statement.json`. A co-signature with the old key (the same command with the old key and `-x statement.json.cosig`) is optional and never enough on its own.
4. Publish the statement with the next release: the release workflow puts `{"body": <base64 of statement.json, byte for byte>, "minisig": <the .minisig text>, "cosig": <optional>}` into the `statements` of every manifest, after all earlier statements in `seq` order. **From then on, every manifest carries the statements** that chain its key to the recovery key, and every freshness statement after a freshness-key rotation carries them too, so a copy that was offline across the rotation can still verify. Never change a published statement's bytes: a different statement with a sequence number already accepted is refused as equivocation.
5. Replace the role's `.pub` file and its CI secret. For a release or nightly rotation, re-sign each affected channel's current manifest with the new key; for a freshness rotation, issue new freshness statements with the new key.
6. Append the published statement to `crates/update/keys/statements.json`, so later builds embed it and deleting `trust.json` can't undo the revocation. Leave the `.pub` copies in `crates/update/keys/` as they are: they are the first key generation, and the statements carry every change after it. A test fails unless each `.pub` file here is its role's current key in the embedded chain.

When the owner first creates the nightly or freshness key, the default is simpler: add its `.pub` file here, copy it into `crates/update/keys/`, and change that role's `None` in `crates/update/src/keys.rs` to its `include_str!` (one line). That makes it part of the first generation, with no statement.

## The recovery key: an accepted risk

The recovery key is offline and password-protected, and **there's no protocol to rotate it.** If it's lost or compromised:
- installed copies can't take a new release key automatically;
- the remedy is a new Lodestar build with a new embedded recovery key, which every user installs **by hand** once.

No recovery-key rotation statement is designed (`docs/owner-decisions.md` §2.21).

A second accepted risk: with an image built **before** a rotation, someone who can write to the data folder and holds the leaked, rotated release key can plant an update that runs. Writing to the data folder already means full control of the installation's data. Updating to a current image closes the gap.

## Decided (`docs/owner-decisions.md` §2.21)

- **The `release` environment is limited to `v*` tags, with the owner as required reviewer.** Every Stable and Beta release waits for one approval before it's signed.
- **Nightlies use a separate key and environment,** trusted only on the Nightly channel. A compromised nightly pipeline can't sign Stable or Beta manifests.
- **A freshness key keeps current manifests from lapsing** without a weekly approval. It can only extend an already-signed manifest's expiry, so a leak can freeze updates but never install or roll back anything.
- **Owner setup:** create the `nightly` and `freshness` key pairs (`minisign -G -W`), add their public keys here, and create the `nightly` and `freshness` environments (main only) with their secrets.
