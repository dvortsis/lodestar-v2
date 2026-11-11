// Tells the status pill whether saved settings wait for a restart, and shows the restart banner
// once one does. The chrome starts it after the first interaction. The banner is mounted into the
// Settings shell's `#restart-slot` (a Settings first view carries that empty element, not the
// banner, its store or its messages) and again after each visit, as the shell is rebuilt.
//
// Asks `GET /system` on start, every 15 s on a Settings screen and once a minute elsewhere; the
// banner shares the same store and refreshes it itself while it is shown.
import { mount, unmount, type Component } from 'svelte';
import { restart } from './restart.svelte';

const SETTINGS_MS = 15_000;
const ELSEWHERE_MS = 60_000;

export function watchRestart(set: (needed: boolean) => void): () => void {
	let asked = Date.now();
	let Banner: Component | undefined;
	let loading = false;
	let host: Element | null = null;
	let shown: Record<string, unknown> | undefined;
	const ask = () => {
		asked = Date.now();
		void restart.refresh();
	};
	const unmountBanner = () => {
		if (shown) void unmount(shown);
		shown = undefined;
		host = null;
	};
	const place = () => {
		const el = document.getElementById('restart-slot');
		if (el === host) return;
		unmountBanner();
		if (el && Banner) {
			host = el;
			shown = mount(Banner, { target: el });
		}
	};
	ask();
	const tick = setInterval(() => {
		const every = location.pathname.startsWith('/settings') ? SETTINGS_MS : ELSEWHERE_MS;
		if (!document.hidden && Date.now() - asked >= every) ask();
		place();
	}, 500);
	const stopEffect = $effect.root(() => {
		$effect(() => {
			const needed = restart.pending !== null;
			set(needed);
			if (needed && !Banner && !loading) {
				loading = true;
				void import('./RestartBanner.svelte').then((mod) => {
					Banner = mod.default;
					place();
				});
			}
		});
	});
	return () => {
		clearInterval(tick);
		stopEffect();
		unmountBanner();
	};
}
