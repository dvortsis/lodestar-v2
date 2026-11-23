<script lang="ts">
	// The app chrome: top bar, toasts and the command palette. It owns the one keydown
	// listener and the event-stream subscription for the status pill (where a 401 goes is
	// Shell.svelte's, since it must not wait for this). The sign-in and first-run screens are `bare`
	// (skip link only, no stream). `Shell.svelte` loads this on the first interaction (or after
	// 30 s), so none of it is on the critical path (§10.1: LCP and the first-load budget).
	import { onMount } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { toasts } from '$lib/ui/feedback/toasts.svelte';
	import { shell } from './shell-state.svelte';
	import { statusRef } from './status-ref.svelte';
	import TopBar from './TopBar.svelte';

	let {
		bare,
		early = [],
		onkeys,
		engaged = false
	}: {
		bare: boolean;
		/** Key presses from before the key listener attached, oldest first (Shell.svelte). */
		early?: KeyboardEvent[];
		/** Called once the key listener is attached: the shell stops keeping presses. */
		onkeys?: () => void;
		/** Mounted because of an interaction (or the shell's timeout): start the deferred parts now. */
		engaged?: boolean;
	} = $props();

	const main = () => document.getElementById('main');

	let Toaster = $state<typeof import('$lib/ui/feedback/Toaster.svelte').default>();
	let Palette = $state<typeof import('./CommandPalette.svelte').default>();
	let Stale = $state<typeof import('./StaleBanner.svelte').default>();

	function loadToaster() {
		if (!Toaster)
			void import('$lib/ui/feedback/Toaster.svelte').then((mod) => (Toaster = mod.default));
	}

	// The toaster (and its icons) load with the first toast or the first interaction, not with the page.
	$effect(() => {
		if (toasts.items.length) loadToaster();
	});

	const ENGAGED = ['pointerdown', 'keydown', 'touchstart', 'focusin'] as const;
	const START_AFTER_MS = 30_000;
	/** A key press kept longer than this before the listener attached is not replayed. */
	const STALE_KEY_MS = 5_000;
	/** Where Enter or Space already does something: the registry's own CONTROL list
	 *  (`$lib/keys/registry.svelte.ts`), kept the same here. Exporting it would add bytes to every page
	 *  that loads the registry, Insights included, which is at its limit. */
	const CONTROL = [
		'a[href]',
		'button',
		'summary',
		'select',
		...['checkbox', 'radio', 'submit', 'button', 'reset', 'image', 'file', 'range', 'color'].map(
			(t) => `input[type="${t}"]`
		),
		...[
			'button',
			'link',
			'menuitem',
			'menuitemcheckbox',
			'menuitemradio',
			'option',
			'tab',
			'checkbox',
			'radio',
			'switch',
			'slider',
			'spinbutton',
			'treeitem',
			'gridcell'
		].map((r) => `[role="${r}"]`)
	].join(', ');

	/** Runs `cb` once: on the first interaction, or after START_AFTER_MS. Returns the cancel. */
	function whenEngaged(cb: () => void): () => void {
		if (engaged) {
			cb();
			return () => {};
		}
		const stop = () => {
			clearTimeout(timer);
			for (const e of ENGAGED) window.removeEventListener(e, go, true);
		};
		const go = () => {
			stop();
			cb();
		};
		const timer = setTimeout(go, START_AFTER_MS);
		for (const e of ENGAGED) window.addEventListener(e, go, { capture: true, passive: true });
		return stop;
	}

	let offStale: (() => void) | undefined;
	let offCommands: (() => void) | undefined;
	let offShortcuts: (() => void) | undefined;
	let offAppearance: (() => void) | undefined;
	let detachKeys: (() => void) | undefined;
	let disposed = false;

	onMount(() => {
		// The 401 redirect is installed by Shell.svelte, before the chrome loads.
		// The rest is not needed to read the page and counts against the load (§10.1): the key
		// shortcuts, the stream and the pill start with the first interaction, or 30 s after load.
		// A click, a tap or a focus move (an assistive technology's cursor too) after a kept key press
		// changes what that press meant: the replay leaves out every press before the latest one.
		// One between the shell's first key and this mount isn't seen (it would cost the first load).
		let moved = 0;
		const move = (e: Event) => (moved = e.timeStamp);
		const unmove = () => {
			removeEventListener('pointerdown', move, true);
			removeEventListener('focusin', move, true);
		};
		addEventListener('pointerdown', move, true);
		addEventListener('focusin', move, true);
		const stopWaiting = whenEngaged(() => {
			void Promise.all([
				import('$lib/keys'),
				import('./commands'),
				import('$lib/features/shortcuts/boot'),
				import('$lib/features/appearance/boot'),
				import('./status.svelte'),
				import('./StatusPill.svelte'),
				import('./ConnectionView.svelte')
			])
				.then(([keysModule, commands, shortcutsBoot, appearanceBoot, status, pill, connection]) => {
					if (disposed) return;
					detachKeys = keysModule.keys.attach(window);
					offCommands = commands.registerShellCommands();
					offShortcuts = shortcutsBoot.startShortcuts();
					offAppearance = appearanceBoot.startAppearance();
					statusRef.Pill = pill.default;
					statusRef.Connection = connection.default;
					statusRef.current = status.shellStatus;
					onkeys?.();
					unmove();
					// The presses made while this loaded, the one that woke us first, now find their shortcuts,
					// in order. They go to the registry only: the page has already seen the real events, and
					// handing it second ones would act twice. Left out:
					// - a press the browser or the page acted on (Tab, Enter or Space on a control, anything
					//   default-prevented), or one before a click or focus move, and every press before it:
					//   replayed now, they would undo it;
					// - the Ctrl/Cmd+K the shell took for the palette, and every press after it: those were
					//   meant for the palette (all of them while it is open);
					// - a press in a field (the text is there), and a plain key once focus is in one (after a
					//   replayed `/`): it was meant as typing;
					// - one kept too long (a stalled load), so a late burst of shortcuts never fires.
					const kept = shell.paletteOpen ? [] : early.splice(0);
					early.length = 0;
					let from = 0;
					let to = kept.length;
					kept.forEach((e, i) => {
						if (i >= to) return;
						if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k' && e.defaultPrevented)
							to = i;
						else if (
							e.defaultPrevented ||
							e.timeStamp < moved ||
							e.key === 'Tab' ||
							((e.key === 'Enter' || e.key === ' ') && (e.target as Element).closest(CONTROL))
						)
							from = i + 1;
					});
					for (const e of kept.slice(from, to))
						if (
							(e.target as Node).isConnected &&
							performance.now() - e.timeStamp < STALE_KEY_MS &&
							!keysModule.isEditable(e.target) &&
							(e.ctrlKey || e.metaKey || e.altKey || !keysModule.isEditable(document.activeElement))
						)
							keysModule.keys.handle(e);
				})
				// A chunk that can't load (a stale build, the server gone): no shortcuts, so the shell stops
				// keeping presses and taking Ctrl/Cmd+K, and nothing is replayed.
				.catch(() => {
					unmove();
					early.length = 0;
					onkeys?.();
				});
			// Fetched now, while the server is up: an error toast and the error screen have to work
			// when it is down.
			loadToaster();
			void import('./ErrorView.svelte');
			// A newer server build is noticed when the tab is shown again (dev builds have none). The
			// banner is fetched now, with the check, while this build's chunks still exist: after an
			// upgrade the server no longer has them. Bare screens (sign-in) can become the app without
			// remounting this, so the check always starts and only the banner waits for `!bare`.
			if (!import.meta.env.DEV)
				void Promise.all([import('./stale'), import('./StaleBanner.svelte')])
					.then(([mod, banner]) => {
						if (!disposed) offStale = mod.watchStale(() => (Stale = banner.default));
					})
					.catch(() => {});
		});
		return () => {
			unmove();
			disposed = true;
			stopWaiting();
			offStale?.();
			detachKeys?.();
			offCommands?.();
			offShortcuts?.();
			offAppearance?.();
		};
	});

	// Stream subscription follows the chrome: none on the bare screens.
	$effect(() => {
		if (bare) return;
		return statusRef.current?.start();
	});

	$effect(() => statusRef.current?.observe());

	// The pill's progress facts (stalled, starting) come from their own small module.
	$effect(() => {
		const status = statusRef.current;
		if (bare || !status) return;
		let stop: (() => void) | undefined;
		let gone = false;
		void import('./progress').then((mod) => {
			if (!gone) stop = mod.startProgress((p) => status.setProgress(p));
		});
		return () => {
			gone = true;
			stop?.();
		};
	});
	// The pill's "Restart needed" (§10.10.6): asks `/system` once a minute, and the Settings banner
	// refreshes it sooner while it is open.
	$effect(() => {
		const status = statusRef.current;
		if (bare || !status) return;
		let stop: (() => void) | undefined;
		let gone = false;
		void import('$lib/features/restart/watch.svelte').then((mod) => {
			if (!gone) stop = mod.watchRestart((needed) => status.setRestartNeeded(needed));
		});
		return () => {
			gone = true;
			stop?.();
		};
	});

	$effect(() => {
		if (shell.paletteOpen && !Palette)
			void import('./CommandPalette.svelte').then((mod) => (Palette = mod.default));
	});

	// A new page starts at the top of the page, not wherever focus was: move it to <main> when the
	// path changes (not for query changes such as typing in search, and not on first load).
	afterNavigate((nav) => {
		if (nav.type === 'enter' || !nav.from || nav.from.url.pathname === nav.to?.url.pathname) return;
		main()?.focus({ preventScroll: true });
	});
</script>

{#if !bare}<TopBar />{/if}
{#if !bare && Stale}<Stale />{/if}
{#if Toaster}<Toaster />{/if}
{#if !bare && Palette}<Palette bind:open={shell.paletteOpen} />{/if}
