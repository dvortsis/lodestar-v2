<script lang="ts">
	// The app chrome: top bar, toasts and the command palette. It owns the one keydown
	// listener, the event-stream subscription for the status pill, and where a 401 goes. The sign-in
	// and first-run screens are `bare` (skip link only, no stream). `Shell.svelte` loads this after
	// the page has painted, so none of it is on the critical path (§10.1: LCP).
	import { onMount } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { toasts } from '$lib/ui/feedback/toasts.svelte';
	import { shell } from './shell.svelte';
	import { statusRef } from './status-ref.svelte';
	import TopBar from './TopBar.svelte';

	let { bare, replay }: { bare: boolean; replay?: { event: KeyboardEvent; target: HTMLElement } } =
		$props();

	const main = () => document.getElementById('main');

	let Toaster = $state<typeof import('$lib/ui/feedback/Toaster.svelte').default>();
	let Palette = $state<typeof import('./CommandPalette.svelte').default>();

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

	/** Runs `cb` once: on the first interaction, or after START_AFTER_MS. Returns the cancel. */
	function whenEngaged(cb: () => void): () => void {
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

	let offCommands: (() => void) | undefined;
	let detachKeys: (() => void) | undefined;
	let uninstallAuth: (() => void) | undefined;
	let disposed = false;

	onMount(() => {
		// The 401 redirect starts as soon as the chrome mounts, so a page's own requests are covered.
		// M-auth's hook owns W5's single `onLoginRequired` slot.
		void import('$lib/features/auth/session.svelte').then((auth) => {
			if (!disposed) uninstallAuth = auth.installAuthHandlers();
		});
		// The rest is not needed to read the page and counts against the load (§10.1): the key
		// shortcuts, the stream and the pill start with the first interaction, or 30 s after load.
		const stopWaiting = whenEngaged(() => {
			void Promise.all([
				import('$lib/keys'),
				import('./commands'),
				import('./status.svelte'),
				import('./StatusPill.svelte'),
				import('./ConnectionView.svelte')
			]).then(([keysModule, commands, status, pill, connection]) => {
				if (disposed) return;
				detachKeys = keysModule.keys.attach(window);
				offCommands = commands.registerShellCommands();
				statusRef.Pill = pill.default;
				statusRef.Connection = connection.default;
				statusRef.current = status.shellStatus;
				// The key press that woke us now finds its shortcut. It goes to the registry only: the page
				// has already seen the real event, and handing it a second one would act twice.
				if (replay?.target.isConnected && !replay.event.defaultPrevented)
					keysModule.keys.handle(replay.event);
			});
			// Fetched now, while the server is up: an error toast has to work when it is down.
			loadToaster();
		});
		return () => {
			disposed = true;
			stopWaiting();
			detachKeys?.();
			offCommands?.();
			uninstallAuth?.();
		};
	});

	// Stream subscription follows the chrome: none on the bare screens.
	$effect(() => {
		if (bare) return;
		return statusRef.current?.start();
	});

	$effect(() => statusRef.current?.observe());

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
{#if Toaster}<Toaster />{/if}
{#if !bare && Palette}<Palette bind:open={shell.paletteOpen} />{/if}
