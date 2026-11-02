import { readFileSync } from 'node:fs';

/**
 * Stable `kit.version.name`: the package version, plus the short commit SHA when CI sets one.
 * SvelteKit derives the `__sveltekit_<id>` app id and `_app/version.json` from this, so the same
 * commit always builds byte-identical JS. Local builds use the package version alone.
 * `LODESTAR_BUILD_VERSION` overrides it outright: the CI size step builds with a constant so
 * the measured bytes don't vary with the commit (the app id and chunk hashes derive from this).
 * @param {Record<string, string | undefined>} [env]
 */
export function buildVersion(env = process.env) {
	const fixed = (env.LODESTAR_BUILD_VERSION ?? '').trim();
	if (fixed) return fixed;
	const pkg = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
	const sha = (env.GITHUB_SHA ?? '').trim().slice(0, 7);
	return sha ? `${pkg.version}-${sha}` : String(pkg.version);
}
