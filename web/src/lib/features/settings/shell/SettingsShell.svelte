<script lang="ts">
	import type { Snippet } from 'svelte';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { withDensity, type Route } from '$lib/shell/nav';
	import IconGear from '~icons/ph/gear';
	import { groups, matchesScreen, screens } from './screens';
	let { children }: { children: Snippet } = $props();
	let query = $state('');
	const overview = $derived(page.url.pathname.replace(/\/$/, '') === '/settings');
	const matches = $derived(screens.filter((screen) => matchesScreen(screen, query)));
	const href = (path: string) => resolve(withDensity(path, page.url.searchParams) as Route);
</script>

<div class="settings-shell p-4 xl:p-6" class:settings-overview={overview}>
	<aside class="settings-navigation flex flex-col gap-6">
		<a
			class="ls-focus text-title font-semibold"
			href={href('/settings')}
			aria-current={overview ? 'page' : undefined}>{m.settings_shell_title()}</a
		>
		<label class="flex flex-col gap-2 text-label">
			{m.settings_shell_filter_sections()}
			<input class="input ls-focus w-full" type="search" bind:value={query} />
		</label>
		<nav class="flex flex-col gap-6" aria-label={m.settings_shell_navigation()}>
			{#each groups as group (group.id)}
				{@const members = matches.filter((screen) => screen.group === group.id)}
				{#if members.length}
					<div class="flex flex-col gap-2">
						<h2 class="text-label text-muted-text">{group.label()}</h2>
						{#each members as screen (screen.id)}
							{@const Icon = screen.icon ?? IconGear}
							<a
								class="settings-link ls-focus flex items-center gap-2 rounded-field px-3 py-2"
								href={href(`/settings/${screen.id}`)}
								aria-current={page.params.section === screen.id ? 'page' : undefined}
							>
								<Icon class="size-5 shrink-0" aria-hidden="true" />{screen.title()}
							</a>
						{/each}
					</div>
				{/if}
			{/each}
			{#if !matches.length}<p role="status">{m.settings_shell_no_sections()}</p>{/if}
		</nav>
	</aside>
	<div class="settings-content flex min-w-0 flex-col gap-6">
		{#if !overview}<a class="settings-back ls-focus text-primary underline" href={href('/settings')}
				>{m.settings_shell_back()}</a
			>{/if}
		{@render children()}
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
	.settings-overview .settings-navigation {
		display: flex;
	}
	.settings-content {
		width: 100%;
		max-width: var(--manuscript-width);
	}
	.settings-link[aria-current='page'] {
		background: var(--color-base-200);
		font-weight: 600;
	}
	@media (pointer: coarse) {
		.settings-link {
			min-height: var(--target-min);
		}
	}
	@media (min-width: 1024px) {
		.settings-shell {
			grid-template-columns: minmax(12rem, 16rem) minmax(0, 1fr);
			align-items: start;
		}
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
