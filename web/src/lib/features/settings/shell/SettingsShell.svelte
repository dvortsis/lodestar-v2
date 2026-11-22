<script lang="ts">
	import { PAGE_BODY_FULL, PAGE_FRAME } from '$lib/ui/layout/classes';
	import type { Snippet } from 'svelte';
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { withDensity, type Route } from '$lib/shell/nav';
	import NavIcon from './NavIcon.svelte';
	import { groups, screens } from './screens';
	import type { Component } from 'svelte';
	let { children }: { children: Snippet } = $props();
	let query = $state('');
	let input = $state<HTMLInputElement>();
	let icons = $state.raw<Record<string, Component>>();
	// Settings search (find.ts, FindPanel.svelte) loads the first time the box is focused, and with
	// it the `/` key; until then, and if it fails to load, the screen list stays as it is.
	let Panel =
		$state.raw<Component<{ input: HTMLInputElement; query: string; clear: () => void }>>();
	let lazy: Promise<typeof import('./find-keys')> | undefined;
	function ask() {
		void lazy?.then((mod) => mod.loadPanel()).then((mod) => (Panel = mod.default));
	}
	onMount(() => {
		let off: (() => void) | undefined;
		let live = true;
		void (lazy = import('./find-keys')).then((mod) => {
			if (!live) return;
			icons = mod.NAV_ICONS;
			off = mod.registerFindKey(() => input?.focus());
		});
		return () => {
			live = false;
			off?.();
		};
	});
	const overview = $derived(page.url.pathname.replace(/\/$/, '') === '/settings');
	const finding = $derived(Panel !== undefined && query.trim() !== '');
	const href = (path: string) => resolve(withDensity(path, page.url.searchParams) as Route);
</script>

<div class={PAGE_FRAME} data-page-frame="">
	<div class={PAGE_BODY_FULL}>
		<div class="settings-shell" class:settings-overview={overview}>
			<aside class="settings-navigation flex flex-col gap-6">
				<a
					class="ls-focus text-title font-semibold"
					href={href('/settings')}
					aria-current={overview ? 'page' : undefined}>{m.settings_shell_title()}</a
				>
				<label class="settings-find flex flex-col gap-2 text-small">
					{m.settings_shell_filter_sections()}
					<input
						class="input ls-focus w-full"
						type="search"
						bind:value={query}
						bind:this={input}
						onfocus={ask}
					/>
				</label>
				{#if Panel && input}
					<Panel {input} bind:query clear={() => (query = '')} />
				{/if}
				{#if !finding}
					<nav class="settings-list flex flex-col gap-6" aria-label={m.settings_shell_navigation()}>
						{#each groups as group (group.id)}
							{@const members = screens.filter((screen) => screen.group === group.id)}
							{#if members.length}
								<div class="flex flex-col gap-2">
									<h2 class="text-small text-muted-text">{group.label()}</h2>
									{#each members as screen (screen.id)}
										<a
											class="settings-link ls-focus flex items-center gap-2 rounded-field px-3 py-2"
											href={href(`/settings/${screen.id}`)}
											aria-current={page.params.section === screen.id ? 'page' : undefined}
										>
											<NavIcon {screen} {icons} />{screen.title()}
										</a>
									{/each}
								</div>
							{/if}
						{/each}
					</nav>
				{/if}
			</aside>
			<div class="settings-content flex min-w-0 flex-col gap-6">
				{#if !overview}<a
						class="settings-back ls-focus text-primary underline"
						href={href('/settings')}>{m.settings_shell_back()}</a
					>{/if}
				{@render children()}
			</div>
		</div>
	</div>
</div>

<style>
	.settings-shell {
		display: grid;
		gap: calc(var(--spacing) * 8);
	}
	.settings-navigation {
		display: none;
	}
	/* Below 1024 px the Overview reads: the search box, what needs attention and what to set, then
	   the list of screens (§10.10.3), so the aside's parts are ordered around the content. */
	.settings-overview .settings-navigation {
		display: contents;
	}
	.settings-find {
		order: 1;
	}
	:global(.settings-list) {
		order: 3;
	}
	.settings-content {
		order: 2;
		width: 100%;
		max-width: var(--manuscript-width);
	}
	.settings-link[aria-current='page'] {
		background: var(--color-selected);
		font-weight: 600;
	}
	@media (min-width: 1024px) {
		.settings-shell {
			grid-template-columns: minmax(12rem, 16rem) minmax(0, 1fr);
			align-items: start;
		}
		.settings-overview .settings-navigation,
		.settings-navigation {
			display: flex;
			position: sticky;
			top: calc(var(--spacing) * 6);
		}
		.settings-back {
			display: none;
		}
	}
</style>
