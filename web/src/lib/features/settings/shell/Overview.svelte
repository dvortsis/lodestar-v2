<script lang="ts">
	// `/settings` (§10.10.3): "Is anything wrong, and what did I change?". Needs attention first,
	// one row per item with one action, then (below x-large) the grouped list, then the settings
	// that differ from their default, counted per screen. A loading state waits 300 ms before it
	// shows, so a fast answer never flashes.
	import { afterNavigate } from '$app/navigation';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import IconInfo from '~icons/ph/info';
	import IconRestart from '~icons/ph/arrows-clockwise';
	import IconWarning from '~icons/ph/warning';
	import type { FieldKey } from '../form/types';
	import { focusHeading } from './focus';
	import SettingsNav from './SettingsNav.svelte';
	import SettingsSearch from './SettingsSearch.svelte';
	import { useSettings } from './state.svelte';

	const settings = useSettings();
	afterNavigate(() => {
		focusHeading();
	});

	const locale = $derived(currentLocale());
	const loading = $derived(
		settings.systemStatus === 'loading' ||
			(settings.systemStatus === 'ready' && !settings.attention.ready)
	);
	const icons = { problem: IconWarning, restart: IconRestart, notice: IconInfo };

	// Show the skeleton only once the wait is noticeable.
	let slow = $state(false);
	$effect(() => {
		if (!loading) {
			slow = false;
			return;
		}
		const timer = setTimeout(() => (slow = true), 300);
		return () => clearTimeout(timer);
	});

	/** The server's list of non-default settings, counted per screen (the client adds nothing up). */
	const changed = $derived.by(() => {
		const counts: Record<string, number> = {};
		for (const c of settings.system?.diagnostics.changed ?? []) {
			const id = settings.registry.screenOf(c.name as FieldKey)?.screen.id;
			if (id) counts[id] = (counts[id] ?? 0) + 1;
		}
		return settings.registry.screens
			.filter((r) => counts[r.screen.id])
			.map((r) => ({ id: r.screen.id, title: r.screen.title(), count: counts[r.screen.id]! }));
	});
</script>

<div class="flex flex-col gap-6" data-testid="settings-overview" aria-busy={loading}>
	<header class="flex flex-col gap-2">
		<h1 tabindex="-1" class="text-display font-semibold outline-none">{m.settings_ui_title()}</h1>
		<p class="text-body text-muted-text">{m.settings_ui_overview_purpose()}</p>
	</header>

	{#if !settings.wide}<SettingsSearch />{/if}

	{#if settings.systemStatus === 'failed'}
		<!-- When the schema failed too, the layout's banner already offers Retry for both. -->
		{#if settings.status !== 'failed'}
			<Banner kind="error">
				{m.settings_ui_overview_system_failed()}
				{#snippet action()}
					<Button variant="secondary" onclick={settings.retry}>{m.settings_ui_retry()}</Button>
				{/snippet}
			</Banner>
		{/if}
	{:else}
		<section class="flex flex-col gap-3" aria-labelledby="overview-attention">
			<h2 id="overview-attention" class="text-title font-semibold">{m.needs_attention()}</h2>
			{#if loading}
				{#if slow}<Skeleton lines={2} />{/if}
			{:else if settings.attention.items.length === 0}
				<p data-testid="settings-attention-none">{m.settings_ui_overview_attention_none()}</p>
			{:else}
				<ul class="flex flex-col gap-2" data-testid="settings-attention">
					{#each settings.attention.items as item (item.id)}
						{@const Icon = icons[item.severity]}
						<li
							class="attention-row"
							data-severity={item.severity}
							data-testid="settings-attention-{item.id}"
						>
							<span class="flex min-w-0 items-start gap-2">
								<Icon class="attention-icon" aria-hidden="true" />
								<span>{item.text()}</span>
							</span>
							{#if item.action.href}
								<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- no base path (svelte.config.js) -->
								<a class="btn btn-outline btn-sm" href={item.action.href}>{item.action.label()}</a>
							{:else}
								<Button dense onclick={() => void item.action.run?.()}>{item.action.label()}</Button
								>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
			{#if settings.attention.partial && !loading}
				<p class="text-small text-muted-text" data-testid="settings-attention-partial">
					{m.settings_ui_overview_partial()}
				</p>
			{/if}
		</section>
	{/if}

	{#if !settings.wide}<SettingsNav
			screens={settings.registry.screens}
			badges={settings.badges}
		/>{/if}

	{#if settings.systemStatus === 'ready'}
		<section class="flex flex-col gap-3" aria-labelledby="overview-changed">
			<h2 id="overview-changed" class="text-title font-semibold">{m.setting_changed()}</h2>
			{#if changed.length === 0}
				<p data-testid="settings-changed-none">{m.settings_ui_overview_changed_none()}</p>
			{:else}
				<ul class="flex flex-col gap-1">
					{#each changed as c (c.id)}
						<li>
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- no base path (svelte.config.js) -->
							<a class="changed-link" href="/settings/{c.id}?changed=1">
								<span>{c.title}</span>
								<span class="text-muted-text" data-testid="settings-changed-{c.id}"
									>{m.settings_ui_overview_changed_count({
										count: formatCount(c.count, locale)
									})}</span
								>
							</a>
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	{/if}
</div>

<style>
	.attention-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: calc(var(--spacing) * 2) calc(var(--spacing) * 3);
		padding: calc(var(--spacing) * 3);
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-box);
		background: var(--color-base-100);
	}
	.attention-row[data-severity='problem'] {
		border-color: var(--color-error);
	}
	:global(.attention-icon) {
		flex: none;
		width: var(--icon-size-row);
		height: var(--icon-size-row);
		margin-top: calc(var(--spacing) * 0.5);
	}
	.changed-link {
		display: flex;
		justify-content: space-between;
		gap: calc(var(--spacing) * 3);
		min-height: var(--control-height);
		align-items: center;
		padding-inline: calc(var(--spacing) * 3);
		border-radius: var(--radius-field);
		color: var(--color-base-content);
	}
	@media (pointer: coarse) {
		.changed-link {
			min-height: var(--target-min);
		}
	}
	.changed-link:hover {
		background: var(--color-base-200);
	}
</style>
