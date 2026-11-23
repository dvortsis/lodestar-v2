import { initLocale, setZone } from '$lib/i18n/locale.svelte';
import { isBareRoute } from '$lib/shell/chrome';
import type { LayoutLoad } from './$types';

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

// One session check at boot (X-setup-token-boot, UI review I14). Before the first page renders, a
// fresh instance goes to setup, and a signed-out visitor (or one whose Legal Notice is pending) to
// sign-in, which returns them here: nobody sees a page they can't use yet. Only the first load
// asks; later navigations meet the client's 401 handler as before. The bare pages (sign-in, setup,
// welcome) run their own check. Every route's first load carries this, so it stays a few bytes
// with no imports of its own (§10.1); the static boot screen shows while it waits.

/**
 * Where the first load of `next` (a path and query) goes instead, or null to stay. Only an explicit
 * answer moves it: a failed, slow (5 s) or unreadable one stays, and the page's own requests still
 * meet the client's 401 handler.
 */
export async function _bootTarget(fetch: typeof globalThis.fetch, next: string) {
	const s: { logged_in?: unknown; notice_required?: unknown; setup_required?: unknown } =
		(await fetch('/api/v1/auth', { signal: AbortSignal.timeout(5e3) })
			.then((r) => (r.ok ? r.json() : 0))
			.catch(() => 0)) || {};
	if (s.setup_required === true) return '/setup';
	// As `loginUrl` (features/auth/next.ts) builds it, inlined: importing it adds a chunk to every
	// first load. The sign-in page reads `next=/` as home.
	if (s.logged_in === false || s.notice_required === true)
		return '/login?next=' + encodeURIComponent(next);
	return null;
}

let booted = false;

export const load: LayoutLoad = async ({ fetch }) => {
	if (booted) return;
	booted = true;
	// The address being loaded; read from `location`, not `url`, so this load depends on nothing and
	// never runs again on a navigation.
	const { pathname, search } = location;
	const to = isBareRoute(pathname) ? null : await _bootTarget(fetch, pathname + search);
	// A full load of the target, in place of this entry (Back doesn't return to a page that would
	// send it on again). Not kit's `redirect`: importing it grows the shared runtime chunk every
	// route loads. Nothing renders meanwhile.
	if (to) {
		location.replace(to);
		await new Promise(() => {});
	}
};
