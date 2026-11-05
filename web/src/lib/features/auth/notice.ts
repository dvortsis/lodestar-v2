// The Legal Notice (owner decision §2.38): fetched from the server, which builds it in, once per
// page load. Setup sends its version with the password; the sign-in gate accepts it.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type LegalNotice = components['schemas']['LegalNotice'];

let cached: Promise<LegalNotice> | undefined;

/** The notice; a failed fetch isn't cached, so the next call tries again. */
export function loadNotice(): Promise<LegalNotice> {
	cached ??= api.get('/legal/notice').catch((e: unknown) => {
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
	return Promise.race([
		fonts.load('400 1em "Atkinson Hyperlegible Next"', 'Lodestar').then(
			() => {},
			() => {}
		),
		new Promise<void>((resolve) => setTimeout(resolve, FONT_WAIT_MS))
	]);
}

/** Records that the signed-in admin accepted `version`. */
export async function acceptNotice(version: number): Promise<void> {
	await api.post('/legal/accept', { body: { version } });
}

/** Tests only. */
export function resetNoticeCache(): void {
	cached = undefined;
}
