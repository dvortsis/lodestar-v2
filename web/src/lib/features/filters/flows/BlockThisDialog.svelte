<script lang="ts">
	// "Block this…" (§20.2): the narrowest rule first, then the normal preview flow with
	// `addRule({ query, action })`. Tiers the data can't support yet are shown disabled with their
	// reason; the phrase tier is never preselected. `hide` is the default action (reversible at
	// once, and it applies to stored torrents). Before the preview, the rule's current match count
	// comes from `/search` (`hidden=true`, `exact=true`).
	import { api, isAbort, latest } from '$lib/api/client';
	import Radio from '$lib/ui/fields/Radio.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { nameWords, phraseQuery, suggestBlocks, type BlockTarget, type BlockTier } from './block';
	import ChangeDialog from './ChangeDialog.svelte';
	import { addRule } from './changes';
	import { createChangeFlow } from './flow.svelte';
	import type { ApplyResponse } from './types';

	interface Props {
		torrent: BlockTarget;
		open: boolean;
		onclose?: () => void;
		onapplied?: (response: ApplyResponse) => void;
	}

	let { torrent, open = $bindable(), onclose, onapplied }: Props = $props();

	const flow = createChangeFlow();
	const uid = $props.id();
	const suggestions = $derived(suggestBlocks(torrent));
	const words = $derived(nameWords(torrent.name));

	let tier = $state<BlockTier | null>(null);
	let action = $state('hide');
	/** The phrase: a run of consecutive words, `[from, to]` inclusive. */
	let range = $state<[number, number] | null>(null);

	// The narrowest available tier is chosen at open; the phrase never is.
	$effect.pre(() => {
		if (!open) return;
		const first = suggestions.find((s) => s.available && s.tier !== 'phrase');
		tier ??= first?.tier ?? null;
	});

	const phrase = $derived(range ? phraseQuery(words.slice(range[0], range[1] + 1)) : null);
	const query = $derived(
		tier === 'phrase' ? phrase : (suggestions.find((s) => s.tier === tier)?.query ?? null)
	);

	function pick(i: number) {
		if (!range) range = [i, i];
		else if (i === range[0] - 1) range = [i, range[1]];
		else if (i === range[1] + 1) range = [range[0], i];
		else if (i === range[0] && range[0] < range[1]) range = [i + 1, range[1]];
		else if (i === range[1] && range[0] < range[1]) range = [range[0], i - 1];
		else if (i === range[0] && i === range[1]) range = null;
		else range = [i, i];
	}

	// What the rule would match now. One request at a time; a stale answer is dropped.
	const count = latest();
	let matches = $state<{ total: number; approx: boolean } | 'failed' | null>(null);
	$effect(() => {
		const q = query;
		matches = null;
		if (!open || !q) return;
		count
			.run((signal) =>
				api.get('/search', { query: { q, hidden: true, exact: true, limit: 1 }, signal })
			)
			.then(
				(r) => (matches = { total: r.total, approx: r.total_approx }),
				(e) => !isAbort(e) && (matches = 'failed')
			);
		return () => count.cancel();
	});

	const actions = $derived([
		{
			value: 'hide',
			label: m.filters_flows_action_hide(),
			help: m.filters_flows_action_hide_help()
		},
		{
			value: 'quarantine',
			label: m.filters_flows_action_quarantine(),
			help: m.filters_flows_action_quarantine_help()
		},
		{
			value: 'drop',
			label: m.filters_flows_action_drop(),
			help: m.filters_flows_action_drop_help()
		}
	]);
	const tierLabels: Record<BlockTier, () => string> = {
		torrent: m.filters_flows_tier_torrent,
		payload: m.filters_flows_tier_payload,
		group: m.filters_flows_tier_group,
		phrase: m.filters_flows_tier_phrase
	};
	const reasons = {
		no_payload_signature: m.filters_flows_reason_no_payload_signature,
		no_group: m.filters_flows_reason_no_group
	};

	function startPreview() {
		if (!query) return;
		flow.start(addRule({ query, action }));
	}

	function reset() {
		tier = null;
		range = null;
		action = 'hide';
		matches = null;
	}
</script>

<ChangeDialog
	{flow}
	bind:open
	title={m.filters_flows_block_title()}
	{onapplied}
	onclose={() => {
		reset();
		onclose?.();
	}}
>
	{#snippet form()}
		<div class="flex flex-col gap-4" data-testid="block-form">
			<p>{m.filters_flows_block_lead()}</p>
			<fieldset class="flex flex-col gap-2">
				<legend class="text-small font-medium">{m.filters_flows_block_what()}</legend>
				{#each suggestions as s (s.tier)}
					<div class="flex flex-col gap-1" data-testid="block-tier-{s.tier}">
						<Radio
							name="{uid}-tier"
							value={s.tier}
							bind:group={tier}
							label={tierLabels[s.tier]()}
							disabled={!s.available}
							aria-describedby={s.reason ? `${uid}-${s.tier}-why` : undefined}
						/>
						{#if s.reason}
							<p id="{uid}-{s.tier}-why" class="text-small text-muted-text">
								{reasons[s.reason]()}
							</p>
						{:else if s.tier === 'torrent' && s.query}
							<Mono wrap class="text-muted-text">{s.query}</Mono>
						{/if}
					</div>
				{/each}
			</fieldset>
			{#if tier === 'phrase'}
				<div class="flex flex-col gap-2" data-testid="block-phrase">
					<p class="text-small font-medium" id="{uid}-words">{m.filters_flows_phrase_pick()}</p>
					<p class="text-small text-muted-text">{m.filters_flows_phrase_help()}</p>
					<div class="flex flex-wrap gap-1" role="group" aria-labelledby="{uid}-words">
						{#each words as w, i (i)}
							<Button
								dense
								variant={range && i >= range[0] && i <= range[1] ? 'primary' : 'secondary'}
								aria-pressed={!!range && i >= range[0] && i <= range[1]}
								onclick={() => pick(i)}
								data-testid="block-word">{w}</Button
							>
						{/each}
					</div>
					{#if range && !phrase}
						<p class="text-small" role="alert">{m.filters_flows_phrase_invalid()}</p>
					{/if}
				</div>
			{/if}
			<fieldset class="flex flex-col gap-2">
				<legend class="text-small font-medium">{m.filters_flows_action()}</legend>
				{#each actions as a (a.value)}
					<div class="flex flex-col gap-0.5">
						<Radio
							name="{uid}-action"
							value={a.value}
							bind:group={action}
							label={a.label}
							aria-describedby="{uid}-{a.value}-help"
							data-testid="block-action-{a.value}"
						/>
						<p id="{uid}-{a.value}-help" class="text-small text-muted-text">{a.help}</p>
					</div>
				{/each}
			</fieldset>
			{#if query}
				<div class="flex flex-col gap-1">
					<span data-testid="block-query"><Mono wrap>{query}</Mono></span>
					<p class="flex justify-between gap-3">
						<span class="text-muted-text">{m.filters_flows_matches_now()}</span>
						<span class="tabular-nums" data-testid="block-matches" aria-live="polite">
							{#if matches === 'failed'}{m.filters_flows_matches_failed()}{:else if matches}{formatCount(
									{ value: matches.total, approx: matches.approx },
									currentLocale()
								)}{:else}—{/if}
						</span>
					</p>
				</div>
			{/if}
		</div>
	{/snippet}
	{#snippet formFooter()}
		<Button
			variant="primary"
			onclick={startPreview}
			aria-disabled={!query || undefined}
			data-testid="block-preview">{m.filters_flows_preview()}</Button
		>
	{/snippet}
</ChangeDialog>
