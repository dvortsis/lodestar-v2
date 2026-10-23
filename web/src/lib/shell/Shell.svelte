<script lang="ts">
	// The app frame (§10.3): the skip link, the main landmark, the connection states and, once the
	// page has painted, the chrome around it. Only this frame is on the critical path; the top bar,
	// event stream, palette and toaster (`Chrome.svelte`) load after first paint into a slot that
	// already has the bar's height, so nothing shifts (§10.1: LCP, CLS). The skip link and the
	// connection states are plain markup here, so they work before the chrome arrives and when the
	// server (and so every lazy chunk) is gone. The page is rendered once, here, and never remounted.
	import { onMount, type Snippet } from 'svelte';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { isBareRoute } from './chrome';
	import { statusRef } from './status-ref.svelte';

	let { children }: { children: Snippet } = $props();

	const bare = $derived(isBareRoute(page.url.pathname));
	let Chrome = $state<typeof import('./Chrome.svelte').default>();
	const connection = $derived(bare ? 'ok' : (statusRef.current?.connection ?? 'ok'));
	const unreachable = $derived(connection === 'unreachable');

	// The top bar's slot. app.html paints a static bar before any script runs (#boot-header); it is
	// moved in here when the app mounts, and the rest of #boot goes, in the same task: one frame,
	// no shift. The chrome replaces it with the full bar a moment later.
	let slot = $state<HTMLElement>();
	// The first key press, kept for the chrome to replay once its shortcuts exist (typing is not).
	let replay = $state<{ event: KeyboardEvent; target: HTMLElement }>();
	const typing = (el: HTMLElement) =>
		el.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName);
	onMount(() => {
		const first = (e: KeyboardEvent) => {
			removeEventListener('keydown', first, true);
			if (e.target instanceof HTMLElement && !typing(e.target))
				replay = { event: e, target: e.target };
		};
		addEventListener('keydown', first, { capture: true, passive: true });
		return () => removeEventListener('keydown', first, true);
	});
	onMount(() => {
		const header = document.getElementById('boot-header');
		// eslint-disable-next-line svelte/no-dom-manipulating -- adopts the static bar app.html painted
		if (header && slot) slot.replaceChildren(header);
		document.getElementById('boot')?.remove();
	});

	onMount(() => {
		// After the first frame, so the request never competes with the page's first paint.
		let timer: ReturnType<typeof setTimeout>;
		const frame = requestAnimationFrame(() => {
			timer = setTimeout(
				() => void import('./Chrome.svelte').then((mod) => (Chrome = mod.default))
			);
		});
		return () => {
			cancelAnimationFrame(frame);
			clearTimeout(timer);
		};
	});

	function skip(e: MouseEvent) {
		e.preventDefault();
		document.getElementById('main')?.focus();
	}
</script>

<div class="flex min-h-dvh flex-col bg-base-200 text-base-content">
	<a
		href="#main"
		onclick={skip}
		class="sr-only rounded-field bg-primary px-4 py-2 text-primary-content focus:not-sr-only focus:fixed focus:start-2 focus:top-2 focus:z-50 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary focus-visible:outline-solid"
		>{m.shell_skip_to_content()}</a
	>
	{#if Chrome}
		<Chrome {bare} {replay} />
	{:else if !bare}
		<!-- Holds the top bar's place (1 px border + 56 px) until it arrives. -->
		<div bind:this={slot}>
			<div class="border-b border-base-300 bg-base-100"><div class="min-h-14"></div></div>
		</div>
	{/if}
	<main id="main" tabindex="-1" class="flex flex-1 flex-col outline-none">
		{#if connection !== 'ok' && statusRef.Connection}
			<statusRef.Connection kind={connection} />
		{/if}
		<!-- Always mounted: hiding it keeps the page's state across a connection blip. -->
		<div class="flex flex-1 flex-col" hidden={unreachable}>
			{@render children()}
		</div>
	</main>
</div>
