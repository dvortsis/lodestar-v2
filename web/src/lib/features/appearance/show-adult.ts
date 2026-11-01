// "Show adult content" on Appearance's first view (X-flows-door-2), without `filters/flows/**`:
// the 18+ confirmation and turning it on load through the flows' door; turning it off asks nothing,
// so it never waits on that load. `show-adult.test.ts` keeps these in step with the flows' own.
import { api, ApiError } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import type { ShowAdultResult } from '../filters/flows/adult.svelte';

/** The variable that sets `content.show_adult` from outside the app (B17): the flows' `SHOW_ADULT_ENV`. */
export const SHOW_ADULT_ENV = 'LODESTAR_SHOW_ADULT';

/** Turns adult content off, as the flows' `setShowAdult(false, …)` does. */
export async function showAdultOff(): Promise<ShowAdultResult> {
	const body = { show_adult: false } as unknown as components['schemas']['SettingsPatch'];
	try {
		await api.patch('/settings/{section}', { path: { section: 'content' }, body });
		return { ok: true };
	} catch (e) {
		const error =
			e instanceof ApiError ? e : new ApiError(0, 'network', e instanceof Error ? e.message : '');
		return { ok: false, error };
	}
}
