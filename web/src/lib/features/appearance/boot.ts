// Applies the account's saved appearance when the shell starts (§10.18: the theme follows the
// account across devices). The pre-paint script has already applied the per-browser cache; this
// fetches `preferences` and corrects the page only where the server differs. A failed fetch leaves
// the pre-paint state alone, and so does a choice the user makes while the fetch is in flight.
import { latest } from '$lib/api/client';
import { events } from '$lib/state/events.svelte';
import { onceLive } from '$lib/features/shortcuts/follow.svelte';
import { applyLook, familyFor, load, prefersDark, type Appearance } from './prefs';
import { THEME_CACHE, applyTheme, savePending, themeGeneration } from './themes';

const FALLBACK_MS = 10_000;
const slot = latest();

const lookShown = (p: Appearance) => {
	const root = document.documentElement.dataset;
	return (
		root.density === p.density &&
		root.motion === p.motion &&
		root.illustrations === String(p.illustrations)
	);
};

/** Whether the page and the pre-paint cache already hold this theme choice, both modes. */
function themeShown(p: Appearance): boolean {
	const dark = prefersDark(p.mode);
	try {
		const c = JSON.parse(localStorage.getItem(THEME_CACHE) ?? 'null') as {
			mode?: string;
			light?: { id?: string };
			dark?: { id?: string };
		} | null;
		return (
			document.documentElement.dataset.theme ===
				`${familyFor(p, dark)}-${dark ? 'dark' : 'light'}` &&
			c?.mode === p.mode &&
			c.light?.id === `${familyFor(p, false)}-light` &&
			c.dark?.id === `${familyFor(p, true)}-dark`
		);
	} catch {
		return false;
	}
}

/** Fetches the saved preferences and applies them. Any failure keeps what is applied. */
export async function refresh(): Promise<void> {
	const before = themeGeneration();
	try {
		const loaded = await slot.run((signal) => load(signal));
		// A choice made on the Appearance screen meanwhile is newer than this answer.
		// A save still pending means the server's answer is the older choice: applying it flickers.
		if (!loaded || themeGeneration() !== before || savePending()) return;
		// The pre-paint script sets the theme only, so the look is set on every start.
		if (!lookShown(loaded.prefs)) applyLook(loaded.prefs);
		// A custom theme this browser lacks falls back to Riverstone inside, without an error.
		if (!themeShown(loaded.prefs)) await applyTheme(loaded.prefs);
	} catch {
		// A 401 is the shell's; offline, superseded or a missing themes file keeps what is applied.
	}
}

export function startAppearance(): () => void {
	let fetched = false;
	const fetchOnce = () => {
		if (!fetched) void refresh();
		fetched = true;
	};
	// Like the shortcuts: not before the shell's stream is live, with a timer as the fallback.
	const timer = setTimeout(() => events.status !== 'stopped' && fetchOnce(), FALLBACK_MS);
	const off = onceLive(() => {
		fetchOnce();
		return events.on(
			'settings',
			(e) => {
				if (e.section === 'preferences') void refresh();
			},
			{ onResync: () => void refresh() }
		);
	});
	return () => {
		clearTimeout(timer);
		off();
		slot.cancel();
	};
}
