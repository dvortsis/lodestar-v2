// Tells the status pill whether saved settings wait for a restart, announces it, and shows the
// restart banner once a change waits (or a new HTTP address waits to be kept). The chrome starts it
// after the first interaction.
//
// - Asks `GET /system` on start, on each route change, when the tab is focused again, every 15 s on a Settings screen and once a
//   minute elsewhere; the banner shares the same store.
// - The banner floats (a fixed tray at the bottom), so it never moves content; it is mounted on the
//   body and checks the route itself.
// - The polite live region is created here, empty, as soon as the watch starts, and filled only
//   when the pending count changes. The pill's own announcement stays quiet for this state, so the
//   two never say it twice.
import { mount, unmount, type Component } from 'svelte';
import { page } from '$app/state';
import { restart } from './restart.svelte';
import { countText } from './text';

const SETTINGS_MS = 15_000;
const ELSEWHERE_MS = 60_000;
const TICK_MS = 5_000;

export function watchRestart(set: (needed: boolean) => void): () => void {
	let asked = Date.now();
	let Banner: Component | undefined;
	let loading = false;
	let stopped = false;
	let shown: Record<string, unknown> | undefined;
	const ask = () => {
		asked = Date.now();
		void restart.refresh();
	};
	const live = document.createElement('div');
	live.className = 'sr-only';
	live.setAttribute('role', 'status');
	live.dataset.testid = 'restart-announce';
	live.setAttribute('data-testid', 'restart-announce');
	document.body.append(live);

	window.addEventListener('focus', ask);
	const tick = setInterval(() => {
		const every = location.pathname.startsWith('/settings') ? SETTINGS_MS : ELSEWHERE_MS;
		if (!document.hidden && Date.now() - asked >= every) ask();
	}, TICK_MS);

	const stopEffect = $effect.root(() => {
		$effect(() => {
			void page.url.pathname;
			ask();
		});
		$effect(() => {
			const pending = restart.pending;
			set(pending !== null);
			live.textContent = countText(pending);
		});
		$effect(() => {
			const awaiting = restart.system?.address_change?.state === 'awaiting_confirmation';
			if ((restart.pending !== null || awaiting) && !Banner && !loading) {
				loading = true;
				void import('./RestartBanner.svelte').then((mod) => {
					Banner = mod.default;
					// Stopped while the chunk was loading: nothing to show it on.
					if (!stopped) shown = mount(Banner, { target: document.body });
				});
			}
		});
	});
	return () => {
		stopped = true;
		clearInterval(tick);
		window.removeEventListener('focus', ask);
		stopEffect();
		if (shown) void unmount(shown);
		live.remove();
	};
}
