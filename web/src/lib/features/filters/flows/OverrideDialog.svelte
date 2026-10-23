<script lang="ts">
	// A CSX-5 entry (§20.5.5): the editor, then the mandatory preview through the normal flow. Only
	// the shape is checked here (hex lengths, non-empty, whole days); the server validates the rest
	// and, because it says `confirm: true` for every entry, the warning and password follow.
	// A keyword entry shows both strengths side by side; switching strength re-previews, so the
	// warning and the password are for the strength chosen.
	import Button from '$lib/ui/controls/Button.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Textarea from '$lib/ui/fields/Textarea.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import ChangeDialog from './ChangeDialog.svelte';
	import { addOverride, type OverrideInput } from './changes';
	import { createChangeFlow } from './flow.svelte';
	import ImpactSummary from './ImpactSummary.svelte';
	import type { ApplyResponse, Assessment, ChangeOf, Preview } from './types';

	interface Props {
		open?: boolean;
		onclose?: () => void;
		onapplied?: (response: ApplyResponse) => void;
		/** Offer "Shadow for N days". */
		shadowDays?: number;
	}

	let { open = $bindable(false), onclose, onapplied, shadowDays }: Props = $props();

	type Kind = 'hashes' | 'keyword' | 'query' | 'payload';
	const flow = createChangeFlow();

	let kind = $state<Kind>('keyword');
	let value = $state('');
	let strength = $state<'override' | 'neutralize'>('override');
	let scope = $state('');
	let expires = $state('');
	let shadow = $state('');
	let tried = $state(false);
	const applying = $derived(flow.state.kind === 'applying');

	/** The server caps expiry and shadow at ten years; zero would expire at once. */
	const MAX_DAYS = 3650;
	const HEX40 = /^[0-9a-f]{40}$/i;
	const HEX16 = /^[0-9a-f]{16}$/i;
	const hashes = $derived(value.split(/[\s,]+/).filter(Boolean));
	const valueError = $derived.by(() => {
		if (!value.trim()) return m.filters_flows_invalid_empty();
		if (kind === 'hashes' && !hashes.every((h) => HEX40.test(h)))
			return m.filters_flows_invalid_hashes();
		if (kind === 'payload' && !HEX16.test(value.trim())) return m.filters_flows_invalid_payload();
		return undefined;
	});
	const days = (s: string) => (s.trim() === '' ? null : /^\d+$/.test(s.trim()) ? Number(s) : NaN);
	const dayError = (s: string) => {
		const d = days(s);
		return d === null || (d >= 1 && d <= MAX_DAYS)
			? undefined
			: m.filters_flows_invalid_days({ max: formatCount(MAX_DAYS, currentLocale()) });
	};

	function entry(neutralize: boolean): ChangeOf<'add_override'> {
		const input: OverrideInput =
			kind === 'hashes'
				? { hashes: hashes.map((h) => h.toLowerCase()) }
				: kind === 'payload'
					? { payload: value.trim().toLowerCase() }
					: kind === 'keyword'
						? { keyword: value.trim() }
						: { query: value.trim() };
		if (kind === 'keyword' && neutralize) input.neutralize = true;
		if (scope.trim()) input.scope = scope.trim();
		const e = days(expires);
		const s = days(shadow);
		if (e !== null) input.expires_days = e;
		if (s !== null) input.shadow_days = s;
		return addOverride(input);
	}

	function startPreview() {
		tried = true;
		if (valueError || dayError(expires) || dayError(shadow)) return;
		overrideCol = null;
		neutralCol = null;
		flow.start(entry(strength === 'neutralize'));
	}

	// The strength chosen is the source of truth: once a preview has settled, a different strength
	// on screen than in the previewed entry re-previews it (switching mid-preview waits for it).
	$effect(() => {
		const s = flow.state;
		if ((s.kind !== 'review' && s.kind !== 'confirm') || s.change.op !== 'add_override') return;
		if (kind !== 'keyword' || !!s.change.neutralize === (strength === 'neutralize')) return;
		// From the change previewed, not the form: a shadow choice made since is kept.
		const next = { ...s.change };
		if (strength === 'neutralize') next.neutralize = true;
		else delete next.neutralize;
		flow.variant(next);
	});

	// The two columns come from the server: an Override preview carries the Neutralize numbers
	// beside it; a Neutralize preview carries only its own, so the Override column is the last one
	// the server sent.
	let overrideCol = $state<Preview | null>(null);
	let neutralCol = $state<Preview | null>(null);
	$effect(() => {
		const s = flow.state;
		if (s.kind !== 'review' && s.kind !== 'confirm') return;
		if (s.change.op !== 'add_override') return;
		if (s.change.neutralize) neutralCol = s.assessment.preview;
		else {
			overrideCol = s.assessment.preview;
			if (s.assessment.neutralize) neutralCol = s.assessment.neutralize;
		}
	});

	const kinds = $derived([
		{ value: 'hashes', label: m.filters_flows_kind_hashes() },
		{ value: 'keyword', label: m.filters_flows_kind_keyword() },
		{ value: 'query', label: m.filters_flows_kind_query() },
		{ value: 'payload', label: m.filters_flows_kind_payload() }
	]);
	const strengths = $derived([
		{ value: 'override', label: m.filters_flows_strength_override() },
		{ value: 'neutralize', label: m.filters_flows_strength_neutralize() }
	]);
	const valueLabel = $derived(
		{
			hashes: m.filters_flows_hashes_label(),
			keyword: m.filters_flows_keyword_label(),
			query: m.filters_flows_query_label(),
			payload: m.filters_flows_payload_label()
		}[kind]
	);
	const valueHelp = $derived(
		kind === 'hashes'
			? m.filters_flows_hashes_help()
			: kind === 'query'
				? m.filters_flows_query_help()
				: kind === 'payload'
					? m.filters_flows_payload_help()
					: undefined
	);

	function reset() {
		value = '';
		scope = '';
		expires = '';
		shadow = '';
		strength = 'override';
		tried = false;
		overrideCol = null;
		neutralCol = null;
	}
</script>

<ChangeDialog
	{flow}
	bind:open
	title={m.filters_flows_override_title()}
	{shadowDays}
	{onapplied}
	onclose={() => {
		reset();
		onclose?.();
	}}
>
	{#snippet form()}
		<form
			class="flex flex-col gap-4"
			novalidate
			onsubmit={(e) => {
				e.preventDefault();
				startPreview();
			}}
			id="override-form"
			data-testid="override-form"
		>
			<div class="flex flex-col gap-1">
				<span class="text-small font-medium">{m.filters_flows_override_kind()}</span>
				<SegmentedControl
					options={kinds}
					bind:value={kind}
					label={m.filters_flows_override_kind()}
					class="flex-wrap"
				/>
			</div>
			<Field label={valueLabel} help={valueHelp} error={tried ? valueError : undefined} required>
				{#snippet children(props)}
					{#if kind === 'hashes'}
						<Textarea
							{...props}
							bind:value
							rows={3}
							mono
							spellcheck="false"
							autocapitalize="off"
							data-testid="override-value"
						/>
					{:else}
						<Input
							{...props}
							bind:value
							mono={kind === 'payload'}
							spellcheck="false"
							autocapitalize="off"
							data-testid="override-value"
						/>
					{/if}
				{/snippet}
			</Field>
			{#if kind === 'keyword'}
				<div class="flex flex-col gap-1">
					<span class="text-small font-medium">{m.filters_flows_strength()}</span>
					<SegmentedControl
						options={strengths}
						bind:value={strength}
						label={m.filters_flows_strength()}
					/>
					<p class="text-small text-muted-text">
						{strength === 'override'
							? m.filters_flows_strength_override_help()
							: m.filters_flows_strength_neutralize_help()}
					</p>
				</div>
			{/if}
			<Field label={m.filters_flows_scope_label()} help={m.filters_flows_scope_help()}>
				{#snippet children(props)}
					<Input
						{...props}
						bind:value={scope}
						spellcheck="false"
						autocapitalize="off"
						data-testid="override-scope"
					/>
				{/snippet}
			</Field>
			<Field label={m.filters_flows_expires_label()} error={tried ? dayError(expires) : undefined}>
				{#snippet children(props)}
					<Input
						{...props}
						bind:value={expires}
						inputmode="numeric"
						data-testid="override-expires"
					/>
				{/snippet}
			</Field>
			<Field
				label={m.filters_flows_shadow_days_label()}
				help={m.filters_flows_shadow_days_help()}
				error={tried ? dayError(shadow) : undefined}
			>
				{#snippet children(props)}
					<Input {...props} bind:value={shadow} inputmode="numeric" data-testid="override-shadow" />
				{/snippet}
			</Field>
		</form>
	{/snippet}
	{#snippet formFooter()}
		<Button variant="primary" type="submit" form="override-form" data-testid="override-preview">
			{m.filters_flows_preview()}
		</Button>
	{/snippet}
	{#snippet extra()}
		<!-- Neutralize is offered once the server has previewed it beside Override (it refuses it
		     for some keywords), and stays while it's the strength chosen. -->
		{#if kind === 'keyword' && (neutralCol || strength === 'neutralize')}
			<div class="flex flex-col gap-1">
				<span class="text-small font-medium">{m.filters_flows_strength()}</span>
				<SegmentedControl
					options={strengths.map((o) => ({ ...o, disabled: applying }))}
					bind:value={strength}
					label={m.filters_flows_strength()}
				/>
				<p class="text-small text-muted-text">
					{strength === 'override'
						? m.filters_flows_strength_override_help()
						: m.filters_flows_strength_neutralize_help()}
				</p>
			</div>
		{/if}
	{/snippet}
	{#snippet summary(a: Assessment)}
		{#if kind === 'keyword' && overrideCol && neutralCol}
			<ImpactSummary preview={overrideCol} neutralize={neutralCol} chosen={strength} />
		{:else}
			<ImpactSummary preview={a.preview} />
		{/if}
	{/snippet}
</ChangeDialog>
