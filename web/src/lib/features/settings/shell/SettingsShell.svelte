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
	import type { Snippet } from 'svelte';
	import SettingsNav from './SettingsNav.svelte';
	import { createSettingsState, provideSettings } from './state.svelte';

	let { children }: { children: Snippet } = $props();

	const settings = provideSettings(createSettingsState());
	const onScreen = $derived(page.params.section !== undefined);
</script>

<div class="settings-frame" data-wide={settings.wide || undefined}>
	{#if settings.wide}
		<div class="settings-pane">
			<SettingsNav screens={settings.registry.screens} />
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
