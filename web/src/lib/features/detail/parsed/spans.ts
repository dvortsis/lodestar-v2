import type { components } from '$lib/api/schema';

export type Identity = components['schemas']['TitleIdentity'];

/** The server's spans, as sent (`2008`, `1-10`), comma-joined; no spans gives null. */
export const joinSpans = (spans: readonly string[]): string | null =>
	spans.length ? spans.join(', ') : null;
