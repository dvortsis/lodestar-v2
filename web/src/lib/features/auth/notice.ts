// The Legal Notice (owner decision §2.38): fetched from the server, which builds it in, once per
// page load. Setup sends its version with the password; the sign-in gate accepts it.
import { api, errorFromResponse } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type LegalNotice = components['schemas']['LegalNotice'];

let cached: Promise<LegalNotice> | undefined;

/**
 * The notice; a failed fetch isn't cached, so the next call tries again. Fetched directly, not
 * through `api`: it's open, so its answer proves nothing about the session, and `api` would mark
 * the tab signed in (a signed-out visitor of /setup, or the sign-in gate before Accept).
 */
export function loadNotice(): Promise<LegalNotice> {
	cached ??= fetch('/api/v1/legal/notice', { headers: { Accept: 'application/json' } })
		.then(async (r) => {
			if (!r.ok) throw await errorFromResponse(r);
			return (await r.json()) as LegalNotice;
		})
		.catch((e: unknown) => {
			cached = undefined;
			throw e;
		});
	return cached;
}

/** The longest the notice waits for the web font before it shows anyway. */
export const FONT_WAIT_MS = 2000;

/**
 * Resolves once the UI font's Latin face has loaded (it's preloaded), or after `FONT_WAIT_MS`. The
 * notice is long text: drawn in the fallback font and then swapped, its lines would move (layout
 * shift), so its box shows the loading line until then. Asking for the face, rather than reading
 * `document.fonts.status`, also covers the moment before its request has started.
 */
export function fontsSettled(): Promise<void> {
	const fonts = typeof document === 'undefined' ? undefined : document.fonts;
	if (!fonts?.load) return Promise.resolve();
	let timer: ReturnType<typeof setTimeout> | undefined;
	return Promise.race([
		fonts.load('400 1em "Atkinson Hyperlegible Next"', 'Lodestar').then(
			() => {},
			() => {}
		),
		new Promise<void>((resolve) => (timer = setTimeout(resolve, FONT_WAIT_MS)))
	]).finally(() => clearTimeout(timer));
}

/** Records that the signed-in admin accepted `version`. */
export async function acceptNotice(version: number): Promise<void> {
	await api.post('/legal/accept', { body: { version } });
}

/** Tests only. */
export function resetNoticeCache(): void {
	cached = undefined;
}
