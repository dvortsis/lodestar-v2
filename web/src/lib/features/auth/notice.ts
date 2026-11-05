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

/** Records that the signed-in admin accepted `version`. */
export async function acceptNotice(version: number): Promise<void> {
	await api.post('/legal/accept', { body: { version } });
}

/** Tests only. */
export function resetNoticeCache(): void {
	cached = undefined;
}
