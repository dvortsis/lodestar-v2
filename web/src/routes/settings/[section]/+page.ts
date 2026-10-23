import { error, isHttpError, redirect } from '@sveltejs/kit';
import { loadSettingsIndex } from '$lib/features/settings/form/schema';
import { ALIASES, buildRegistry, registered } from '$lib/features/settings/shell/registry';

// A registered id opens at once. An alias (`backup`, `import`) redirects. Anything else may be an
// interim screen, which only the schema knows, so the index decides; if it can't be loaded the
// layout says so with Retry, and the page waits for it.
export async function load({ params }) {
	const alias = Object.hasOwn(ALIASES, params.section) ? ALIASES[params.section] : undefined;
	if (alias) redirect(307, alias);
	if (registered.some((s) => s.id === params.section)) return {};
	try {
		const index = await loadSettingsIndex();
		if (!buildRegistry(index).get(params.section)) error(404, 'Not found');
	} catch (e) {
		if (isHttpError(e)) throw e;
	}
	return {};
}
