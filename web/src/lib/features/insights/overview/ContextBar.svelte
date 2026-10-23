<script lang="ts">
	// The global context bar (§10.9): range, compare, last updated and the crawler's pause and
	// intensity. It stays usable when the overview fails to load. Compare is a parameter, not a
	// filter; every non-default range is a chip that resets it.
	import IconPause from '~icons/ph/pause';
	import IconPlay from '~icons/ph/play';
	import IconError from '~icons/ph/x-circle';
	import Button from '$lib/ui/controls/Button.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import Chip from '$lib/ui/labels/Chip.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { formatDate } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import {
		compareAllowed,
		customFromDays,
		dayOf,
		lastDay,
		type InsightsQuery,
		type RangeId
	} from './query';
	import type { CrawlerAction, CrawlerStatus, Intensity } from './state.svelte';

	interface Props {
		query: InsightsQuery;
		/** `range.tz` and `updated_at` of the loaded overview. */
		tz: string;
		updatedAt: number | undefined;
		crawler: CrawlerStatus | undefined;
		busy: CrawlerAction | undefined;
		crawlerError: { action: CrawlerAction; message: string } | undefined;
		/** A server `400` for the custom range, shown on its field. */
		fieldError: { field: string; message: string } | undefined;
		onrange: (range: Exclude<RangeId, 'custom'>) => void;
		oncustom: (from: number, to: number) => void;
		oncompare: (on: boolean) => void;
		onpause: () => void;
		onresume: () => void;
		onintensity: (level: Intensity) => void;
	}
	let {
		query,
		tz,
		updatedAt,
		crawler,
		busy,
		crawlerError,
		fieldError,
		onrange,
		oncustom,
		oncompare,
		onpause,
		onresume,
		onintensity
	}: Props = $props();

	const locale = $derived(currentLocale());
	let shown = $state<RangeId>('7d');
	let editing = $state(false);
	let fromDay = $state('');
	let toDay = $state('');
	let localError = $state<'from' | 'to' | undefined>();
	let compare = $state(false);

	// The URL is the state: follow it (Back and Forward included).
	const queryKey = $derived(JSON.stringify(query));
	$effect(() => {
		const q = JSON.parse(queryKey) as InsightsQuery;
		shown = q.range;
		editing = q.range === 'custom';
		compare = q.compare;
		if (q.range === 'custom' && q.from !== undefined && q.to !== undefined) {
			fromDay = dayOf(q.from);
			toDay = lastDay(q.to);
		}
	});

	const ranges = $derived([
		{ value: '24h', label: m.insights_overview_range_24h() },
		{ value: '7d', label: m.insights_overview_range_7d() },
		{ value: '30d', label: m.insights_overview_range_30d() },
		{ value: 'all', label: m.insights_overview_range_all() },
		{ value: 'custom', label: m.insights_overview_range_custom() }
	]);
	const levels = $derived([
		{ value: 'low', label: m.insights_overview_intensity_low() },
		{ value: 'balanced', label: m.insights_overview_intensity_balanced() },
		{ value: 'aggressive', label: m.insights_overview_intensity_aggressive() }
	]);
	const intensityEffect = $derived(
		crawler
			? {
					low: m.insights_overview_intensity_low_effect(),
					balanced: m.insights_overview_intensity_balanced_effect(),
					aggressive: m.insights_overview_intensity_aggressive_effect()
				}[crawler.intensity]
			: ''
	);
	const levelName = $derived(levels.find((l) => l.value === crawler?.intensity)?.label ?? '');
	const canCompare = $derived(compareAllowed({ ...query, range: shown }));

	function pick(value: string) {
		shown = value as RangeId;
		if (value === 'custom') {
			editing = true;
			if (!fromDay) fromDay = toDay = dayOf(Date.now() / 1000);
			return;
		}
		editing = false;
		onrange(value as Exclude<RangeId, 'custom'>);
	}

	function apply(event: SubmitEvent) {
		event.preventDefault();
		const r = customFromDays(fromDay, toDay);
		if (!r) {
			localError = fromDay ? 'to' : 'from';
			return;
		}
		localError = undefined;
		oncustom(r.from, r.to);
	}

	function cancel() {
		shown = query.range;
		editing = query.range === 'custom';
		localError = undefined;
	}

	const chip = $derived(
		query.range === 'custom' && query.from !== undefined && query.to !== undefined
			? m.insights_overview_chip_custom({
					from: formatDate(query.from, locale, { timeZone: 'UTC' }),
					to: formatDate(query.to - 1, locale, { timeZone: 'UTC' })
				})
			: query.range === 'all'
				? m.insights_overview_chip_all()
				: query.range === '24h'
					? m.insights_overview_chip_24h()
					: query.range === '30d'
						? m.insights_overview_chip_30d()
						: ''
	);
	const actionText = {
		pause: () => m.insights_overview_action_pause(),
		resume: () => m.insights_overview_action_resume(),
		intensity: () => m.insights_overview_action_intensity()
	};
	const errorFor = (field: 'from' | 'to') =>
		fieldError?.field === field
			? fieldError.message
			: localError === field
				? m.insights_overview_custom_invalid()
				: undefined;
</script>

<div class="ins-bar" data-testid="insights-bar">
	<div class="ins-row">
		<SegmentedControl
			options={ranges}
			bind:value={shown}
			label={m.insights_overview_range_label()}
			onchange={pick}
		/>
		<Toggle
			label={m.insights_overview_compare()}
			bind:checked={compare}
			disabled={!canCompare}
			aria-describedby={canCompare ? undefined : 'ins-compare-why'}
			onchange={() => oncompare(compare)}
			data-testid="insights-compare"
		/>
		{#if !canCompare}
			<span id="ins-compare-why" class="ins-muted" data-testid="insights-compare-why"
				>{m.insights_overview_compare_unavailable()}</span
			>
		{/if}
		{#if chip}
			<Chip
				removeLabel={m.insights_overview_chip_remove({ range: chip })}
				onremove={() => onrange('7d')}
			>
				<span data-testid="insights-range-chip">{chip}</span>
			</Chip>
		{/if}
		<span class="ins-grow ins-muted" data-testid="insights-updated">
			{m.insights_overview_updated()}
			<RelativeTime seconds={updatedAt} timeZone={tz} />
		</span>
		<span class="ins-muted" data-testid="insights-tz">{m.insights_overview_tz({ tz })}</span>
	</div>

	{#if editing}
		<form class="ins-custom" onsubmit={apply} data-testid="insights-custom">
			{#each [['from', m.insights_overview_custom_from()], ['to', m.insights_overview_custom_to()]] as [field, text] (field)}
				{@const err = errorFor(field as 'from' | 'to')}
				<div class="ins-field">
					<label for="ins-custom-{field}">{text}</label>
					<Input
						id="ins-custom-{field}"
						type="date"
						dense
						value={field === 'from' ? fromDay : toDay}
						oninput={(e) => {
							const v = (e.currentTarget as HTMLInputElement).value;
							if (field === 'from') fromDay = v;
							else toDay = v;
						}}
						aria-invalid={err ? true : undefined}
						aria-describedby={err ? `ins-custom-${field}-error` : undefined}
						data-testid="insights-custom-{field}"
					/>
					{#if err}
						<span
							id="ins-custom-{field}-error"
							class="ins-error"
							role="alert"
							data-testid="insights-custom-{field}-error"
							><IconError class="ins-icon-s" aria-hidden="true" />{err}</span
						>
					{/if}
				</div>
			{/each}
			<div class="ins-field">
				<span aria-hidden="true">&nbsp;</span>
				<div class="ins-row">
					<Button type="submit" variant="primary" dense>{m.insights_overview_custom_apply()}</Button
					>
					<Button variant="quiet" dense onclick={cancel}
						>{m.insights_overview_custom_cancel()}</Button
					>
				</div>
			</div>
		</form>
	{/if}

	<div class="ins-row" data-testid="insights-crawler">
		{#if crawler}
			{#if crawler.paused}
				<Button
					variant="primary"
					dense
					loading={busy === 'resume'}
					onclick={onresume}
					data-testid="insights-resume"
				>
					<IconPlay class="ins-icon-s" aria-hidden="true" />{m.insights_overview_resume()}
				</Button>
			{:else}
				<Button
					variant="secondary"
					dense
					loading={busy === 'pause'}
					onclick={onpause}
					data-testid="insights-pause"
				>
					<IconPause class="ins-icon-s" aria-hidden="true" />{m.insights_overview_pause()}
				</Button>
			{/if}
			{#if crawler.intensity_env}
				<span data-testid="insights-intensity-locked">
					{m.insights_overview_intensity_locked({
						level: levelName,
						variable: crawler.intensity_env
					})}
				</span>
			{:else}
				{#key crawler}
					<SegmentedControl
						options={levels}
						value={crawler.intensity}
						label={m.insights_overview_intensity_label()}
						dense
						onchange={(v) => onintensity(v as Intensity)}
					/>
				{/key}
			{/if}
			<span class="ins-muted ins-sentence" data-testid="insights-intensity-effect"
				>{intensityEffect}</span
			>
		{:else}
			<span class="ins-muted">{m.insights_overview_crawler_unknown()}</span>
		{/if}
		{#if crawlerError}
			<span class="ins-error" role="alert" data-testid="insights-crawler-error">
				<IconError class="ins-icon-s" aria-hidden="true" />
				{m.insights_overview_crawler_failed({ action: actionText[crawlerError.action]() })}
				{crawlerError.message}
			</span>
		{/if}
	</div>
</div>
