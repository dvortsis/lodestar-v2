<script lang="ts">
	// Top bar (§10.3): mark and name, the five destinations, status pill, command palette and the
	// overflow menu. Below x-large the destinations collapse behind a menu button that discloses
	// them under the bar (a disclosure, so nothing traps focus). Heavy widgets (the overflow menu,
	// the palette) load on first use to keep the first load within budget (§10.1).
	import { afterNavigate } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { isMacPlatform } from '$lib/keys/combo';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import Compass from './Compass.svelte';
	import Glyph from './Glyph.svelte';
	import { DESTINATIONS, isActive, withDensity, type Route } from './nav';
	import { shell } from './shell.svelte';
	import { statusRef } from './status-ref.svelte';

	let navOpen = $state(false);
	afterNavigate(() => (navOpen = false));
	const hrefFor = (href: string) => withDensity(href, page.url.searchParams);
	const link =
		'relative inline-flex min-h-(--control-height) items-center gap-2 rounded-field px-3 text-body hover:bg-hover';

	// The overflow menu: a plain button until pressed, then the real menu, opened. Its chunk (Bits UI
	// and positioning, ~35 KB) is fetched on hover or focus, never at load.
	type MoreComponent = typeof import('./OverflowMenu.svelte').default;
	let More = $state<MoreComponent>();
	let moreLoad: Promise<MoreComponent> | undefined;
	const loadMore = () => (moreLoad ??= import('./OverflowMenu.svelte').then((mod) => mod.default));
	async function openMore() {
		More = await loadMore();
	}

	// Esc closes the disclosure and returns focus to its button.
	function onkeydown(e: KeyboardEvent) {
		if (e.key === 'Escape' && navOpen) {
			navOpen = false;
			document.getElementById('menu-button')?.focus();
		}
	}
</script>

{#snippet destinations(vertical: boolean)}
	{#each DESTINATIONS as d (d.id)}
		{@const current = isActive(d, page.url.pathname)}
		<a
			href={resolve(hrefFor(d.href) as Route)}
			aria-current={current ? 'page' : undefined}
			data-testid="nav-{d.id}"
			onclick={() => (navOpen = false)}
			class="{link} {FOCUS_RING} {TARGET_MIN} {current ? 'font-semibold' : ''} {vertical
				? 'w-full'
				: ''}"
		>
			{d.label()}
			{#if current}
				<span
					class="absolute {vertical
						? 'inset-y-1 start-0 w-0.5'
						: 'inset-x-3 -bottom-1 h-0.5'} rounded-selector bg-primary"
					aria-hidden="true"
				></span>
			{/if}
		</a>
	{/each}
{/snippet}

<svelte:window {onkeydown} />

<header class="border-b border-base-300 bg-base-100">
	<div class="flex min-h-14 items-center gap-2 px-4">
		<div class="x-large:hidden">
			<button
				type="button"
				id="menu-button"
				class="btn btn-ghost btn-square {FOCUS_RING} {TARGET_MIN}"
				aria-label={navOpen ? m.shell_menu_close() : m.shell_menu_open()}
				aria-expanded={navOpen}
				aria-controls="nav-panel"
				data-testid="menu-button"
				onclick={() => (navOpen = !navOpen)}
			>
				<Glyph name={navOpen ? 'x' : 'list'} />
			</button>
		</div>

		<a
			href={resolve(hrefFor('/search') as Route)}
			aria-label={m.shell_home_label()}
			class="inline-flex items-center gap-2 rounded-field pe-2 {FOCUS_RING} {TARGET_MIN}"
		>
			<Compass class="size-8" />
			<span class="hidden text-body-lg font-semibold large:inline">Lodestar</span>
		</a>

		<nav aria-label={m.shell_nav_label()} class="ms-2 hidden items-center gap-1 x-large:flex">
			{@render destinations(false)}
		</nav>

		<div class="ms-auto flex items-center gap-1">
			{#if statusRef.Pill && statusRef.current}
				<statusRef.Pill state={statusRef.current.pill} />
			{:else}
				<!-- Until the status module loads: the same pill, "Connecting". -->
				<a
					href={resolve('/insights')}
					data-testid="status-pill"
					data-state="connecting"
					class="inline-flex min-h-(--control-height-dense) items-center gap-2 rounded-field px-2 text-small whitespace-nowrap hover:bg-hover {FOCUS_RING} {TARGET_MIN}"
				>
					<Glyph name="dots-three" class="size-(--icon-size-row) shrink-0" />
					<span class="sr-only large:not-sr-only" data-testid="status-pill-text"
						>{m.shell_status_connecting()}</span
					>
					<span class="sr-only">{m.shell_status_label()}</span>
				</a>
			{/if}
			<button
				type="button"
				class="btn btn-ghost btn-square {FOCUS_RING} {TARGET_MIN}"
				aria-label={m.shell_palette_open()}
				aria-keyshortcuts={isMacPlatform() ? 'Meta+K' : 'Control+K'}
				data-testid="palette-button"
				onclick={() => (shell.paletteOpen = true)}
			>
				<Glyph name="command" />
			</button>
			{#if More}
				<More openNow {hrefFor} />
			{:else}
				<button
					type="button"
					class="btn btn-ghost btn-square {FOCUS_RING} {TARGET_MIN}"
					aria-label={m.shell_more_label()}
					aria-haspopup="menu"
					aria-expanded="false"
					data-testid="more-button"
					onclick={openMore}
					onpointerenter={loadMore}
					onfocus={loadMore}
				>
					<Glyph name="dots-three" />
				</button>
			{/if}
		</div>
	</div>
	{#if navOpen}
		<nav
			id="nav-panel"
			aria-label={m.shell_nav_label()}
			class="flex flex-col gap-1 border-t border-base-300 p-2 x-large:hidden"
		>
			{@render destinations(true)}
		</nav>
	{/if}
</header>
