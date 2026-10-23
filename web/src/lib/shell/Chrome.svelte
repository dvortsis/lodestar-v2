<script lang="ts">
	// The app chrome: skip link, top bar, toasts and the command palette. It owns the one keydown
	// listener, the event-stream subscription for the status pill, and where a 401 goes. The sign-in
	// and first-run screens are `bare` (skip link only, no stream). `Shell.svelte` loads this after
	// the page has painted, so none of it is on the critical path (§10.1: LCP).
	import { onMount } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { keys } from '$lib/keys';
	import { toasts } from '$lib/ui/feedback/toasts.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { shell } from './shell.svelte';
	import { statusRef } from './status-ref.svelte';
	import TopBar from './TopBar.svelte';

	let { bare }: { bare: boolean } = $props();

	const main = () => document.getElementById('main');

	let Toaster = $state<typeof import('$lib/ui/feedback/Toaster.svelte').default>();
	let Palette = $state<typeof import('./CommandPalette.svelte').default>();

	const idle = (cb: () => void) =>
		typeof requestIdleCallback === 'function'
			? requestIdleCallback(cb, { timeout: 1000 })
			: setTimeout(cb, 200);

	// The toaster (and its icons) load with the first toast, not with the page.
	$effect(() => {
		if (toasts.items.length && !Toaster)
			void import('$lib/ui/feedback/Toaster.svelte').then((mod) => (Toaster = mod.default));
	});

	let offCommands: (() => void) | undefined;

	onMount(() => {
		const detach = keys.attach(window);
		// Not needed to paint the page. The client and the stream arrive together; from then on a
		// 401 on any request goes to `handleLoginRequired` (the stream's own 401 covers the gap).
		idle(() => {
			void Promise.all([
				import('./status.svelte'),
				import('./StatusPill.svelte'),
				import('$lib/api/client'),
				import('./handoff'),
				import('./commands')
			]).then(([status, pill, client, handoff, commands]) => {
				client.onLoginRequired(handoff.handleLoginRequired);
				offCommands = commands.registerShellCommands();
				statusRef.Pill = pill.default;
				statusRef.current = status.shellStatus;
			});
		});
		return () => {
			detach();
			offCommands?.();
			void import('$lib/api/client').then((client) => client.onLoginRequired(undefined));
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

	function skip(e: MouseEvent) {
		e.preventDefault();
		main()?.focus();
	}
</script>

<a
	href="#main"
	onclick={skip}
	class="sr-only rounded-field bg-primary px-4 py-2 text-primary-content focus:not-sr-only focus:fixed focus:start-2 focus:top-2 focus:z-50 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary focus-visible:outline-solid"
	>{m.shell_skip_to_content()}</a
>
{#if !bare}<TopBar />{/if}
{#if Toaster}<Toaster />{/if}
{#if !bare && Palette}<Palette bind:open={shell.paletteOpen} />{/if}
