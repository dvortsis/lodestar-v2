# Release signing keys

Lodestar's updates are signed with [minisign](https://jedisct1.github.io/minisign/) (Ed25519). These are the **public** keys. The binary embeds them, and every update is checked against them before it runs (architecture §12.2). The private keys are never in this repository.

| File | Key ID | Role | Private key kept |
|---|---|---|---|
| `lodestar-release.pub` | `02910813433E81E0` | Signs every release manifest | In the GitHub Actions environment `release` as `RELEASE_SIGNING_KEY`, which only `v*` tags and `main` can use; plus the owner's offline backup |
| `lodestar-recovery.pub` | `67922DDF005FD310` | Signs **key changes** only, never releases | Offline only, password-protected, with the owner |

## Verifying a release by hand

```
minisign -V -p packaging/keys/lodestar-release.pub -m release.json
```

## Replacing the release key

A key change is a signed statement that names the new release public key and carries a **sequence number** one higher than the last accepted statement. Installed copies accept it only under these rules (§12.2):

- **The recovery key must sign it.** The current release key may co-sign, but its signature alone is never accepted. A leaked release key therefore can't rotate to an attacker's key.
- **Accepting it revokes the previous release key** for manifests.
- **A statement whose sequence number isn't higher than the last one accepted is refused,** so an old statement can't be replayed.

To rotate:
1. Generate a new key pair (`minisign -G -W`).
2. Write the key-change statement: the new public key, the next sequence number and the date.
3. Sign the statement with the **recovery** key, and with the old release key too if it's still trusted.
4. Publish the statement with the next release's manifest.
5. Replace `lodestar-release.pub` and the CI secret.

If the recovery key itself is lost, installed copies can't take a new release key automatically, and users must update by hand once.

## Recommended hardening (owner actions, not yet decided)

These are recommendations for the owner, not decisions:
- **Require a reviewer on the `release` environment, and limit it to `v*` tags.** Every stable release then waits for one approval before it's signed. Nightly builds from `main` would use a separate environment.
- **Use a separate nightly key,** trusted only on the Nightly channel. A compromised nightly pipeline then can't sign Stable or Beta manifests. Its public key would be added here and embedded with a channel restriction.
