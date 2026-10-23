<script lang="ts">
	// The Settings frame (§10.10.2). From x-large: two panes, a sticky navigation beside the
	// manuscript column. Below: list, then detail, one in the DOM at a time. `/settings` is the
	// list (the Overview renders it), a screen opens full width with a Back link. The restart and
	// address banners (part 2) sit above the page in both.
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import IconBack from '~icons/ph/arrow-left';
	import { events } from '$lib/state/events.svelte';
	import type { Component, Snippet } from 'svelte';
	import { flow } from './flow.svelte';
	import SettingsNav from './SettingsNav.svelte';
	import SettingsSearch from './SettingsSearch.svelte';
	import { createSettingsState, provideSettings } from './state.svelte';

	let { children }: { children: Snippet } = $props();

	const settings = provideSettings(createSettingsState());
	const onScreen = $derived(page.params.section !== undefined);

	// `/system` follows the `settings` events: saves change which fields wait for a restart.
	$effect(() =>
		events.on('settings', () => void settings.refreshSystem(), {
			onResync: () => void settings.refreshSystem()
		})
	);

	// `/` focuses the search box while on a Settings page. The key registry loads on demand (as the
	// shell does), and its scope is popped with the page.
	$effect(() => {
		let off: (() => void) | undefined;
		let gone = false;
		void import('$lib/keys').then(({ keys }) => {
			if (gone) return;
			const pop = keys.pushScope('settings');
			let unregister: (() => void) | undefined;
			try {
				unregister = keys.register({
					id: 'settings.search.focus',
					title: () => m.settings_ui_search_focus(),
					keys: ['/'],
					scope: 'settings',
					palette: false,
					run: () => document.getElementById('settings-search')?.focus()
				});
			} catch {
				// Registered already (a hot reload); the page still works without the key.
			}
			off = () => {
				unregister?.();
				pop();
			};
		});
		return () => {
			gone = true;
			off?.();
		};
	});

	// The banners and the restarting state load only when there is something to show.
	let Banners = $state.raw<Component>();
	const needBanners = $derived(
		flow.phase !== 'idle' ||
			(settings.system !== undefined &&
				(settings.system.restart_required.length > 0 ||
					settings.system.address_change?.state === 'awaiting_confirmation'))
	);
	$effect(() => {
		if (needBanners && !Banners)
			void import('./Banners.svelte').then((mod) => (Banners = mod.default));
	});
</script>

<div class="settings-frame" data-wide={settings.wide || undefined}>
	{#if settings.wide}
		<div class="settings-pane">
			<SettingsSearch />
			<SettingsNav screens={settings.registry.screens} badges={settings.badges} />
		</div>
	{/if}
	<div class="settings-main">
		{#if !settings.wide && onScreen}
			<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- no base path (svelte.config.js) -->
			<a class="settings-back" href="/settings" data-testid="settings-back">
				<IconBack aria-hidden="true" />{m.settings_ui_back()}
			</a>
		{/if}
		{#if settings.status === 'failed'}
			<Banner kind="error">
				{m.settings_ui_index_failed()}
				{#snippet action()}
					<Button variant="secondary" onclick={settings.retry}>{m.settings_ui_retry()}</Button>
				{/snippet}
			</Banner>
		{/if}
		{#if Banners && needBanners}<Banners />{/if}
		{@render children()}
	</div>
</div>

<style>
	.settings-frame {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
		width: 100%;
		margin-inline: auto;
		padding: calc(var(--spacing) * 6) calc(var(--spacing) * 4);
	}
	.settings-main {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
		min-width: 0;
	}
	.settings-back {
		display: inline-flex;
		align-items: center;
		align-self: flex-start;
		gap: calc(var(--spacing) * 2);
		min-height: var(--control-height);
		color: var(--color-base-content);
	}
	@media (pointer: coarse) {
		.settings-back {
			min-height: var(--target-min);
		}
	}
	/* x-large and wider (--breakpoint-x-large, 1024 px): two panes. */
	@media (min-width: 64rem) {
		.settings-frame {
			flex-direction: row;
			align-items: flex-start;
			gap: calc(var(--spacing) * 8);
			max-width: calc(var(--rail-width) + var(--manuscript-width) + var(--spacing) * 24);
		}
		.settings-pane {
			position: sticky;
			top: calc(var(--spacing) * 6);
			flex: none;
			width: var(--rail-width);
			max-height: calc(100dvh - var(--spacing) * 12);
			overflow-y: auto;
		}
		.settings-main {
			flex: 1;
			max-width: var(--manuscript-width);
		}
	}
</style>
