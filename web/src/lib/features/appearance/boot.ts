// Applies the account's saved appearance when the shell starts (§10.18: the theme follows the
// account across devices). The pre-paint script has already applied the per-browser cache; this
// fetches `preferences` and corrects the page only where the server differs. A failed fetch leaves
// the pre-paint state alone, and so does a choice the user makes while the fetch is in flight.
import { latest } from '$lib/api/client';
import { events } from '$lib/state/events.svelte';
import { onceLive } from '$lib/features/shortcuts/follow.svelte';
import { applyLook, familyFor, load, prefersDark, type Appearance } from './prefs';
import { applyTheme, themeGeneration } from './themes';

const FALLBACK_MS = 10_000;
const slot = latest();

/** Whether the page already shows these preferences, so nothing needs touching. */
function applied(p: Appearance): boolean {
	const root = document.documentElement;
	const dark = prefersDark(p.mode);
	return (
		root.dataset.theme === `${familyFor(p, dark)}-${dark ? 'dark' : 'light'}` &&
		root.dataset.density === p.density &&
		root.dataset.motion === p.motion &&
		root.dataset.illustrations === String(p.illustrations)
	);
}

/** Fetches the saved preferences and applies them. Any failure keeps what is applied. */
export async function refresh(): Promise<void> {
	const before = themeGeneration();
	try {
		const loaded = await slot.run((signal) => load(signal));
		// A choice made on the Appearance screen meanwhile is newer than this answer.
		if (!loaded || themeGeneration() !== before || applied(loaded.prefs)) return;
		applyLook(loaded.prefs);
		// A custom theme this browser lacks falls back to Riverstone inside, without an error.
		await applyTheme(loaded.prefs);
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
