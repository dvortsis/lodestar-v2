<script lang="ts">
	// The app frame (§10.3): the skip link, the main landmark, the connection states and, once the
	// page has painted, the chrome around it. Only this frame is on the critical path; the top bar,
	// event stream, palette and toaster (`Chrome.svelte`) load after first paint into a slot that
	// already has the bar's height, so nothing shifts (§10.1: LCP, CLS). The skip link and the
	// connection states are plain markup here, so they work before the chrome arrives and when the
	// server (and so every lazy chunk) is gone. The page is rendered once, here, and never remounted.
	import { onMount, tick, type Snippet } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { isBareRoute } from './chrome';
	import { shell } from './shell.svelte';
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
		// The palette's modifier is Cmd on a Mac and Ctrl elsewhere (Ctrl+K in a Mac text field deletes
		// to the end of the line); the same rule as lib/keys/combo.ts.
		const mac = /mac|iphone|ipad/i.test(
			(navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData
				?.platform ?? navigator.platform
		);
		const first = (e: KeyboardEvent) => {
			// Holding Ctrl or Shift is the start of a combo, not the key to replay.
			if (['Control', 'Shift', 'Alt', 'Meta', 'AltGraph'].includes(e.key)) return;
			removeEventListener('keydown', first, true);
			// The palette key is taken here, not left to the registry that loads on this very press:
			// by then the browser has already used Ctrl/Cmd+K itself (the address bar).
			if (!bare && (mac ? e.metaKey : e.ctrlKey) && e.key.toLowerCase() === 'k') {
				e.preventDefault();
				shell.paletteOpen = true;
				return;
			}
			if (e.target instanceof HTMLElement && !typing(e.target))
				replay = { event: e, target: e.target };
		};
		addEventListener('keydown', first, { capture: true });
		return () => removeEventListener('keydown', first, true);
	});
	// Keyboard focus survives the swaps. A user who pressed Tab before the app loaded is on the static
	// skip link or a static bar link; both are replaced (by this frame's skip link at mount, by the
	// full bar when the chrome arrives) and kit resets focus to <body> after it hydrates. So focus
	// is remembered by identity (the skip link, or the data-testid the static and full bars share)
	// and put back on the new element, only while focus is still on <body>.
	let want: string | undefined;
	const SKIP = 'skip';
	function focusKey(el: Element | null): string | undefined {
		if (!(el instanceof HTMLElement)) return undefined;
		if (el.matches('#boot > a:first-child, a[href="#main"]')) return SKIP;
		if (el.closest('header')) return el.dataset.testid;
		return undefined;
	}
	function regain(last: boolean) {
		const key = want;
		if (!key) return;
		const active = document.activeElement;
		if (active && active !== document.body) {
			want = undefined;
			return;
		}
		const target =
			key === SKIP
				? document.querySelector<HTMLElement>('a[href="#main"]')
				: document.querySelector<HTMLElement>(`header [data-testid="${key}"]`);
		if (!target) return;
		// Kept after the first restore until kit's own reset has happened (`last`), so it can be redone.
		if (last) want = undefined;
		target.focus({ preventScroll: true });
	}
	onMount(() => {
		const header = document.getElementById('boot-header');
		const boot = document.getElementById('boot');
		want = focusKey(document.activeElement);
		// eslint-disable-next-line svelte/no-dom-manipulating -- adopts the static bar app.html painted
		if (header && slot) slot.replaceChildren(header);
		boot?.remove();
		void tick().then(() => regain(false));
	});
	afterNavigate((nav) => {
		if (nav.type === 'enter') setTimeout(() => regain(true));
	});

	onMount(() => {
		// After the first frame, so the request never competes with the page's first paint.
		let timer: ReturnType<typeof setTimeout>;
		const frame = requestAnimationFrame(() => {
			timer = setTimeout(
				() =>
					void import('./Chrome.svelte').then(async (mod) => {
						want = focusKey(document.activeElement) ?? want;
						Chrome = mod.default;
						await tick();
						regain(false);
					})
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
