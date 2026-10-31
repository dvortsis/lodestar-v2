import { initLocale, setZone } from '$lib/i18n/locale.svelte';

export const ssr = false;
export const prerender = false;

// The cached language applies before the first paint (a per-browser hint; the server stays
// authoritative). The saved values are fetched on the first interaction, so the
// first load stays small (§10.1); the time zone and first day read the same hint when first used.
let hint: Record<string, unknown> | null = null;
try {
	hint = JSON.parse(localStorage.getItem('lodestar.region') ?? 'null');
} catch {
	// No storage: the browser's language, zone and week.
}
const text = (v: unknown) => (typeof v === 'string' ? v : '');
initLocale(text(hint?.language) || null);
setZone(text(hint?.timeZone));
const wake = () => void import('$lib/i18n/boot').then((m) => m.bootRegion());
addEventListener('pointerdown', wake, { once: true });
addEventListener('keydown', wake, { once: true });
// A passive viewer (a wall display) still syncs; bootRegion runs its fetch only once.
setTimeout(wake, 30_000);
