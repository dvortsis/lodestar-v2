# X-size-deterministic: measure bundle sizes with a fixed build version

**Decision.** `web/scripts/build-version.mjs` honours `LODESTAR_BUILD_VERSION`, which overrides the package version and commit SHA. The `web.yml` size step now rebuilds with `LODESTAR_BUILD_VERSION=0000000` before `npx size-limit`.

**Why.** The commit SHA in `kit.version.name` changes the app id and chunk hashes, so identical source measured differently per commit (`/t/[hash]` 21.12–21.14 kB here, ~30 B in CI). That failed size checks by chance (see #357).

**Shipped vs measured.** The shipped build (`npm run build`, Docker, release) is unchanged and keeps the real SHA. The size build differs only in the version string, which `_app/version.json` and the app id derive from. Nothing else reads it: `shell/stale.ts` and About use `/_app/version.json` at runtime, and `LODESTAR_BUILD_VERSION` is unset in shipped builds, so "new version available" is unaffected. The size build is never shipped.

**Verified.** Two builds with different `GITHUB_SHA`: before, `/t/[hash]` 21.12 vs 21.14 kB, Compare 28.83 vs 28.84 kB, Discover 41.24 vs 41.25 kB. With the override, the full `size-limit` output is byte-identical. No limit was changed.
