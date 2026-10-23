<script lang="ts">
	// "What changed" (§20.10): the server's assessment of the applied change beside the one that was
	// previewed, plus whatever the response counted or created.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import PreviewTable from './PreviewTable.svelte';
	import type { ApplyResponse, Assessment } from './types';

	interface Props {
		response: ApplyResponse;
		previewed: Assessment;
	}

	let { response, previewed }: Props = $props();

	const facts = $derived(
		(
			[
				['released', m.filters_flows_done_released(), response.released],
				['removed', m.filters_flows_done_removed(), response.removed],
				['deleted', m.filters_flows_done_deleted(), response.deleted],
				['settled', m.filters_flows_done_settled(), response.settled],
				['id', m.filters_flows_done_id(), response.id],
				['exception', m.filters_flows_done_exception(), response.exception],
				['version', m.filters_flows_done_version(), response.version]
			] as const
		).filter(([, , v]) => v !== undefined && v !== null)
	);
</script>

<div class="flex flex-col gap-4">
	<p>{m.filters_flows_done_lead()}</p>
	<PreviewTable
		caption={m.filters_flows_done_title()}
		testid="changed"
		columns={[
			{ key: 'previewed', label: m.filters_flows_col_previewed(), preview: previewed.preview },
			{ key: 'applied', label: m.filters_flows_col_applied(), preview: response.assessment.preview }
		]}
	/>
	{#if facts.length}
		<dl class="flex flex-col gap-1">
			{#each facts as [key, label, value] (key)}
				<div class="flex justify-between gap-3">
					<dt class="text-muted-text">{label}</dt>
					<dd class="tabular-nums" data-testid="changed-{key}">
						{key === 'id' || key === 'exception' || key === 'version'
							? String(value)
							: formatCount(value, currentLocale())}
					</dd>
				</div>
			{/each}
		</dl>
	{/if}
</div>
