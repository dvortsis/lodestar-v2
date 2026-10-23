<script lang="ts">
	// The drawer the search page mounts (§10.5, §10.15). It shows while `page.state.detail` is set
	// and places itself: in flow from xx-large up, an overlay at x-large and large, and a full
	// screen view with Back below large. The browser's Back (popstate) clears the state and closes it.
	import { onMount, untrack } from 'svelte';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Drawer from '$lib/ui/overlays/Drawer.svelte';
	import { showBidi } from '$lib/util/bidi';
	import { tokenPx } from '$lib/ui/overlays/motion';
	import DetailActions from './DetailActions.svelte';
	import DetailView from './DetailView.svelte';
	import { setDrawerTarget } from './target';
	import { DetailModel } from './model.svelte';
	import { closeDetail, markClosed, returnFocusTarget } from './open';
	import './detail.css';

	const STORAGE_KEY = 'lodestar.drawer.width';

	type Mode = 'inline' | 'overlay' | 'narrow' | 'full';
	const queries = {
		inline: '(min-width: 1366px)',
		overlay: '(min-width: 1024px)',
		narrow: '(min-width: 640px)'
	};
	let mode = $state<Mode>('inline');

	function readWidth(): number | undefined {
		try {
			const n = Number(localStorage.getItem(STORAGE_KEY));
			return Number.isFinite(n) && n > 0 ? n : undefined;
		} catch {
			return undefined;
		}
	}
	let stored = $state<number | undefined>(readWidth());
	function saveWidth(width: number) {
		stored = width;
		try {
			localStorage.setItem(STORAGE_KEY, String(width));
		} catch {
			// private mode or blocked storage: the width just doesn't persist
		}
	}

	const hash = $derived(page.state.detail?.hash ?? null);
	const open = $derived(hash !== null);
	const model = new DetailModel();
	let announce = $state('');
	// A new Drawer per open: its own `open` state can never disagree with the page state.
	let session = $state(0);
	let wasOpen = false;
	let everOpened = false;
	let openedWith = '';

	onMount(() => {
		// The key commands load after mount; they pull in the magnet code only when one runs.
		void import('./commands').then((mod) => mod.registerDetailCommands());
		if (typeof matchMedia !== 'function') return;
		const lists = (Object.entries(queries) as [keyof typeof queries, string][]).map(
			([name, q]) => [name, matchMedia(q)] as const
		);
		const update = () => {
			mode = lists.find(([, l]) => l.matches)?.[0] ?? 'full';
		};
		update();
		lists.forEach(([, l]) => l.addEventListener('change', update));
		return () => {
			lists.forEach(([, l]) => l.removeEventListener('change', update));
			model.destroy();
			setDrawerTarget(null);
		};
	});

	$effect(() => {
		if (hash) {
			untrack(() => {
				if (!wasOpen) {
					// Reopened: a fresh Drawer (the first one is already fresh).
					if (everOpened) session++;
					everOpened = true;
					openedWith = hash;
				}
				wasOpen = true;
				model.load(hash);
			});
			setDrawerTarget(hash);
		} else {
			model.destroy();
			setDrawerTarget(null);
			markClosed();
			// Closed by the browser's Back: focus has nowhere to be, so it goes back to the invoker.
			if (wasOpen) {
				wasOpen = false;
				queueMicrotask(() => {
					if (document.activeElement === document.body || !document.activeElement) {
						const target = returnFocusTarget();
						if (target?.isConnected) target.focus();
					}
				});
			}
		}
	});

	// A different torrent in an open drawer (j/k) swaps in place; its title is announced once. The
	// first torrent of a session isn't: focus lands on its title, which the reader hears anyway.
	let announced = '';
	$effect(() => {
		const title = model.title;
		if (!open || !title || hash === openedWith || announced === hash) return;
		announced = hash ?? '';
		announce = m.detail_announce({ title });
	});

	// In flow: the width the viewer chose. Overlays use their tokens; full screen takes the container.
	// Back and "Back to Search" close like the drawer's own X: focus returns to the invoker.
	function back() {
		const target = returnFocusTarget();
		closeDetail();
		if (target?.isConnected) target.focus();
	}

	const width = $derived(
		mode === 'full'
			? 100_000
			: mode === 'narrow'
				? tokenPx('--drawer-overlay-width-narrow', 480)
				: mode === 'overlay'
					? undefined
					: stored
	);
</script>

<div class="ls-detail-host" data-mode={mode}>
	{#key session}
		<Drawer
			{open}
			title={showBidi(model.title) || m.torrent()}
			closeLabel={m.detail_close()}
			resizeLabel={m.detail_resize()}
			{width}
			minWidth={mode === 'full' ? 0 : undefined}
			maxWidth={mode === 'full' ? 100_000 : undefined}
			onWidthChange={mode === 'inline' ? saveWidth : undefined}
			onClose={closeDetail}
			returnFocus={returnFocusTarget}
		>
			{#snippet actions()}<DetailActions detail={model.detail} />{/snippet}
			{#if mode === 'full'}
				<div class="ls-detail-row">
					<Button dense onclick={back}>{m.detail_back()}</Button>
				</div>
			{/if}
			<DetailView {model} mode="drawer" onBack={back} />
		</Drawer>
	{/key}
	<div class="sr-only" aria-live="polite" data-testid="detail-announce">{open ? announce : ''}</div>
</div>
