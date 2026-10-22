<script lang="ts">
	// The app frame: skip link, top bar, the main landmark, toasts and the command palette. It owns
	// the one keydown listener, the event-stream subscription for the status pill, and where a 401
	// goes. The sign-in and first-run screens render bare (no chrome, no stream).
	//
	// Weight (§10.1): only the frame and top bar are in the first load. The API client, the event
	// stream, the toaster, the palette and the connection states load when the browser is idle or
	// when first needed.
	import { onMount, type Snippet } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { keys } from '$lib/keys';
	import { m } from '$lib/paraglide/messages.js';
	import { isBareRoute } from './chrome';
	import { shell } from './shell.svelte';
	import { statusRef } from './status-ref.svelte';
	import TopBar from './TopBar.svelte';

	let { children }: { children: Snippet } = $props();

	let main: HTMLElement | undefined = $state();
	const bare = $derived(isBareRoute(page.url.pathname));

	let Toaster = $state<typeof import('$lib/ui/feedback/Toaster.svelte').default>();
	let Palette = $state<typeof import('./CommandPalette.svelte').default>();
	let Connection = $state<typeof import('./ConnectionView.svelte').default>();
	const connection = $derived(statusRef.current?.connection ?? 'ok');

	const idle = (cb: () => void) =>
		typeof requestIdleCallback === 'function'
			? requestIdleCallback(cb, { timeout: 1000 })
			: setTimeout(cb, 200);

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
			void import('$lib/ui/feedback/Toaster.svelte').then((mod) => (Toaster = mod.default));
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
		if (connection !== 'ok' && !Connection)
			void import('./ConnectionView.svelte').then((mod) => (Connection = mod.default));
	});

	$effect(() => {
		if (shell.paletteOpen && !Palette)
			void import('./CommandPalette.svelte').then((mod) => (Palette = mod.default));
	});

	// A new page starts at the top of the page, not wherever focus was: move it to <main> when the
	// path changes (not for query changes such as typing in search, and not on first load).
	afterNavigate((nav) => {
		if (nav.type === 'enter' || !nav.from || nav.from.url.pathname === nav.to?.url.pathname) return;
		main?.focus({ preventScroll: true });
	});

	function skip(e: MouseEvent) {
		e.preventDefault();
		main?.focus();
	}
</script>

<div class="flex min-h-dvh flex-col bg-base-200 text-base-content">
	<a
		href="#main"
		onclick={skip}
		class="sr-only rounded-field bg-primary px-4 py-2 text-primary-content focus:not-sr-only focus:fixed focus:start-2 focus:top-2 focus:z-50 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary focus-visible:outline-solid"
		>{m.shell_skip_to_content()}</a
	>
	{#if !bare}<TopBar />{/if}
	<main id="main" tabindex="-1" bind:this={main} class="flex flex-1 flex-col outline-none">
		{#if !bare && connection === 'unreachable'}
			{#if Connection}<Connection kind="unreachable" />{/if}
		{:else}
			{#if !bare && connection === 'reconnecting' && Connection}
				<Connection kind="reconnecting" />
			{/if}
			{@render children()}
		{/if}
	</main>
	{#if Toaster}<Toaster />{/if}
</div>
{#if !bare && Palette}<Palette bind:open={shell.paletteOpen} />{/if}
