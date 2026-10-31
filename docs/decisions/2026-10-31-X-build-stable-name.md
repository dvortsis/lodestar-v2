# X-build-stable-name decisions

- **`kit.version.name` is `<package version>[-<short SHA>]`, not the build time.** `web/scripts/build-version.mjs` reads `package.json` and appends the first 7 characters of `$GITHUB_SHA` when set. Reason: the default (build time) changed hashed chunk names and the `__sveltekit_<id>` app id on every build, so size-limit entries moved by tens of bytes between identical builds. SvelteKit derives the app id from this name, so it is deterministic too.
- **No workflow change.** GitHub Actions sets `GITHUB_SHA` in every job, including `web.yml`, `e2e.yml` and `release.yml`. Local and dev builds omit it and use the package version alone.
- **Update detection still works.** `_app/version.json` and the server's reported UI build id now change per commit in CI and release builds, which is when a new release exists. Local builds of different commits share one id, so a browser running a local build is not told to reload; that matters only for developers. The app does not use the `updated` store, and `pollInterval` is unchanged.
- **Measured** (two builds of one commit, exact bytes via `size-limit --json`): before, 47 entries differed by up to 51 B; after, 0 do. No limits were changed.
- **Size depends on the SHA's characters,** so an entry within a few bytes of its limit can still move between different commits, never between rebuilds of one commit.
