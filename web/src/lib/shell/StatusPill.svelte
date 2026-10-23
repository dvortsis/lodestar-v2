<script lang="ts">
	// Crawler and index state, icon + text, linking to Insights (§10.3). Narrow widths keep the icon
	// and hide the words visually; they stay in the accessible name. Numbers come from the server
	// through the formatters; unknown is "—".
	import type { Component } from 'svelte';
	import IconBroadcast from '~icons/ph/broadcast';
	import IconPause from '~icons/ph/pause-circle';
	import IconRebuild from '~icons/ph/arrows-clockwise';
	import IconWarning from '~icons/ph/warning';
	import IconOffline from '~icons/ph/wifi-slash';
	import IconUnknown from '~icons/ph/circle-dashed';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import { formatCompact, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import type { Route } from './nav';
	import type { PillKind, PillState } from './status';

	let { state, href = '/insights' }: { state: PillState; href?: string } = $props();

	const icons: Record<PillKind, Component> = {
		unknown: IconUnknown,
		connecting: IconUnknown,
		reconnecting: IconOffline,
		busy: IconWarning,
		crawling: IconBroadcast,
		idle: IconPause,
		paused: IconPause,
		rebuilding: IconRebuild,
		failed: IconWarning
	};
	const words: Record<PillKind, () => string> = {
		unknown: () => m.shell_status_unknown(),
		connecting: () => m.shell_status_connecting(),
		reconnecting: () => m.shell_status_reconnecting(),
		busy: () => m.shell_status_busy(),
		crawling: () => m.shell_status_crawling(),
		idle: () => m.shell_status_idle(),
		paused: () => m.shell_status_paused(),
		rebuilding: () => m.shell_status_rebuilding(),
		failed: () => m.shell_status_failed()
	};

	const Icon = $derived(icons[state.kind]);
	const locale = $derived(currentLocale());
	const showsCount = $derived(
		state.kind === 'crawling' || state.kind === 'idle' || state.kind === 'paused'
	);
	const count = $derived(
		state.kind === 'rebuilding'
			? m.shell_status_rebuild_progress({
					done: formatCount(state.done, locale),
					total: formatCount(state.total, locale)
				})
			: showsCount
				? (state.torrents === 1 ? m.shell_status_torrents_one : m.shell_status_torrents)({
						count: formatCompact(state.torrents, locale)
					})
				: ''
	);
</script>

<a
	href={resolve(href as Route)}
	data-testid="status-pill"
	data-state={state.kind}
	class="inline-flex min-h-(--control-height-dense) items-center gap-2 rounded-field px-2 text-small whitespace-nowrap hover:bg-hover {FOCUS_RING} {TARGET_MIN}"
>
	<Icon class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
	<span class="sr-only large:not-sr-only">
		<span data-testid="status-pill-text">{words[state.kind]()}</span>
		{#if count}<span class="text-muted-text tabular-nums" data-testid="status-pill-count"
				>· {count}</span
			>{/if}
	</span>
	<span class="sr-only">{m.shell_status_label()}</span>
</a>
