<script lang="ts">
	// The drawer the search page mounts (§10.5, §10.15). It shows while `page.state.detail` is set
	// and places itself: in flow from xx-large up, an overlay at x-large and large, and a full
	// screen view with Back below large. The browser's Back (popstate) clears the state and closes it.
	import { onMount, onDestroy, tick, untrack } from 'svelte';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Drawer from '$lib/ui/overlays/Drawer.svelte';
	import { showBidi } from '$lib/util/bidi';
	import { tokenPx } from '$lib/ui/overlays/motion';
	import DetailActions from './DetailActions.svelte';
	import DetailView from './DetailView.svelte';
	import { setDrawerTarget } from './target';
	import { coverBackground } from './fullscreen';
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
	let mode = $state<Mode>('overlay');
	let gridWidth = $state(0);
	let gridOverhead = $state(0);
	let viewportMode = $state<Mode>('overlay');

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
	let container: HTMLDivElement;
	let gone = false;
	let closure = 0;
	const focusTarget = () => {
		const target = returnFocusTarget();
		return target?.isConnected
			? target
			: document.querySelector<HTMLElement>('[data-testid="query-editor"]');
	};
	onDestroy(() => {
		gone = true;
		model.destroy();
		setDrawerTarget(null);
		markClosed();
	});

	onMount(() => {
		// The key commands load after mount; they pull in the magnet code only when one runs.
		// A failed chunk load only leaves the shortcuts unregistered; the page works without them.
		void import('./commands').then(
			(mod) => mod.registerDetailCommands(),
			() => {}
		);
		if (typeof matchMedia !== 'function') return;
		const lists = (Object.entries(queries) as [keyof typeof queries, string][]).map(
			([name, q]) => [name, matchMedia(q)] as const
		);
		const update = () => {
			viewportMode = lists.find(([, l]) => l.matches)?.[0] ?? 'full';
		};
		const searchGrid = container.closest<HTMLElement>('.search-grid');
		const grid = searchGrid ?? container.parentElement;
		const measure = () => {
			if (!grid) return;
			const style = getComputedStyle(grid);
			gridWidth = grid.clientWidth;
			gridOverhead =
				parseFloat(style.paddingLeft) +
				parseFloat(style.paddingRight) +
				(searchGrid ? 2 : 1) * (parseFloat(style.columnGap) || 0) +
				(searchGrid ? tokenPx('--rail-width', 264) : 0) +
				tokenPx('--results-min-width', 560);
		};
		const observer = new ResizeObserver(measure);
		if (grid) observer.observe(grid);
		measure();
		update();
		lists.forEach(([, l]) => l.addEventListener('change', update));
		return () => {
			observer.disconnect();
			lists.forEach(([, l]) => l.removeEventListener('change', update));
		};
	});

	$effect(() => {
		const chosen = Math.min(
			tokenPx('--drawer-max-width', 640),
			Math.max(tokenPx('--drawer-min-width', 480), stored ?? tokenPx('--drawer-overlay-width', 560))
		);
		mode =
			viewportMode === 'inline' && gridWidth < gridOverhead + chosen ? 'overlay' : viewportMode;
	});

	$effect(() => {
		if (!open || mode !== 'full' || !container) return;
		const restore = coverBackground(container);
		// Resizing an already open desktop drawer must move hidden focus into it too.
		if (!container.contains(document.activeElement))
			container.querySelector<HTMLElement>('h2[tabindex]')?.focus();
		return restore;
	});

	$effect(() => {
		if (hash) {
			closure++;
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
				const closed = ++closure;
				void tick().then(() => {
					if (gone || closed !== closure) return;
					const active = document.activeElement;
					// The outgoing drawer can still be mounted for its transition. Restore now
					// if it owns focus; leave any control the user chose outside it alone.
					if (!active || active === document.body || container?.contains(active))
						focusTarget()?.focus();
				});
			}
		}
	});

	// A different torrent in an open drawer (j/k) swaps in place; its title is announced once. The
	// first torrent of a session isn't: focus lands on its title, which the reader hears anyway.
	let announced = '';
	$effect(() => {
		const title = model.title;
		if (!open || !title || !model.hitDone || hash === openedWith || announced === hash) return;
		announced = hash ?? '';
		announce = m.detail_announce({ title });
	});

	// In flow: the width the viewer chose. Overlays use their tokens; full screen takes the container.
	// Back and "Back to Search" close like the drawer's own X: focus returns to the invoker.
	function back() {
		const target = focusTarget();
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

<div bind:this={container} class="ls-detail-host" data-mode={mode}>
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
			returnFocus={focusTarget}
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
