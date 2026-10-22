# Release signing keys

Lodestar's updates are signed with [minisign](https://jedisct1.github.io/minisign/) (Ed25519). These are the **public** keys. The binary embeds them, and every update is checked against them before it runs (architecture §12.2). The private keys are never in this repository.

| File | Key ID | Role | Private key kept |
|---|---|---|---|
| `lodestar-release.pub` | `02910813433E81E0` | Signs every release manifest | In the GitHub Actions environment `release` as `RELEASE_SIGNING_KEY`, which only `v*` tags and `main` can use; plus the owner's offline backup |
| `lodestar-recovery.pub` | `67922DDF005FD310` | Signs only a replacement release key, if the release key is lost or leaked. Never signs releases. | Offline only, password-protected, with the owner |

## Verifying a release by hand

```
minisign -V -p packaging/keys/lodestar-release.pub -m release.json
```

## Replacing the release key

1. Generate a new key pair (`minisign -G -W`).
2. Write a key-change statement naming the new public key and the version from which it takes effect. Sign the statement with the **recovery** key, and also with the old release key if it's still available.
3. Publish the statement with the next release, and replace `lodestar-release.pub` and the CI secret.

Installed copies accept the new key only through a statement that chains to a key built into them (§12.2). If the recovery key itself is lost, installed copies can't take a new release key automatically, and users must update by hand once.
