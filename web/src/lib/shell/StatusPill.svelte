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
	import IconConnecting from '~icons/ph/circle-notch';
	import IconStarting from '~icons/ph/hourglass-medium';
	import IconStalled from '~icons/ph/warning-octagon';
	import IconRestart from '~icons/ph/power';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import { formatCompact, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { statusInk } from '$lib/ui/status';
	import Tooltip from '$lib/ui/overlays/Tooltip.svelte';
	import type { Route } from './nav';
	import { useFleet } from '$lib/features/insights/workers/fleet.svelte';
	import type { PillKind, PillState } from './status';

	let { state, href = '/insights' }: { state: PillState; href?: string } = $props();

	const icons: Record<PillKind, Component> = {
		unknown: IconUnknown,
		connecting: IconConnecting,
		reconnecting: IconOffline,
		busy: IconWarning,
		crawling: IconBroadcast,
		idle: IconPause,
		paused: IconPause,
		rebuilding: IconRebuild,
		failed: IconWarning,
		starting: IconStarting,
		stalled: IconStalled,
		restart: IconRestart
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
		failed: () => m.shell_status_failed(),
		starting: () => m.shell_status_starting(),
		stalled: () => m.shell_status_stalled(),
		restart: () => m.shell_status_restart()
	};
	// States that carry an explanation: a tooltip on the link and a polite announcement (the
	// words alone say "Stalled"; this says what it means and where to look).
	const help: Partial<Record<PillKind, () => string>> = {
		restart: () => m.shell_status_restart_help(),
		starting: () =>
			state.reason === 'restart_record'
				? m.shell_status_record_help()
				: m.shell_status_starting_help(),
		stalled: () =>
			state.reason === 'index_lag' ? m.shell_status_lag_help() : m.shell_status_stalled_help()
	};

	const locale = $derived(currentLocale());
	const showsCount = $derived(
		state.kind === 'crawling' ||
			state.kind === 'restart' ||
			state.kind === 'idle' ||
			state.kind === 'paused' ||
			state.kind === 'starting' ||
			state.kind === 'stalled'
	);
	// The fleet (WK-17): " · N workers · X/h" after the count, with paused and offline when above zero.
	const fleet = useFleet(() => showsCount);

	const Icon = $derived(icons[state.kind]);
	const helpText = $derived(help[state.kind]?.() ?? '');
	const tone = $derived(state.kind === 'stalled' ? statusInk.warning : '');
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

{#snippet link(props: Record<string, unknown>)}
	<a
		{...props}
		href={resolve((state.kind === 'restart' ? '/settings' : href) as Route)}
		data-testid="status-pill"
		data-state={state.kind}
		data-reason={state.reason}
		class="inline-flex min-h-(--control-height-dense) items-center gap-2 rounded-field px-2 text-small whitespace-nowrap hover:bg-hover {FOCUS_RING} {TARGET_MIN} {tone}"
	>
		<Icon class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
		<span class="sr-only large:not-sr-only">
			<span data-testid="status-pill-text">{words[state.kind]()}</span>
			{#if count}<span class="text-muted-text tabular-nums" data-testid="status-pill-count"
					>· {count}</span
				>{/if}
			{#if fleet.text}<span class="text-muted-text tabular-nums" data-testid="status-pill-fleet"
					>· {fleet.text}</span
				>{/if}
		</span>
		<span class="sr-only">{m.shell_status_label()}</span>
	</a>
{/snippet}

{#if helpText}
	<Tooltip side="bottom" content={helpText}>
		{#snippet trigger({ props })}{@render link(props)}{/snippet}
	</Tooltip>
{:else}
	{@render link({})}
{/if}
<!-- Always present, so a change into "Stalled" or "Starting" is announced; empty otherwise. -->
<span class="sr-only" role="status" data-testid="status-pill-announce">{helpText}</span>
