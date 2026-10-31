import { bootRegion } from '$lib/i18n/boot';

export const ssr = false;
export const prerender = false;

// The cached language, time zone and first day apply now; the saved values follow (not awaited).
void bootRegion();
