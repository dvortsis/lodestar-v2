<script lang="ts">
	// The exposure summary (§20.10): what the change would do, from the server's preview. With
	// `neutralize` (a CSX-5 keyword entry), both strengths side by side: Override | Neutralize term.
	import { m } from '$lib/paraglide/messages.js';
	import PreviewTable from './PreviewTable.svelte';
	import type { Preview } from './types';

	interface Props {
		preview: Preview;
		/** The same entry as Neutralize term; `preview` is then the Override column. */
		neutralize?: Preview | null;
	}

	let { preview, neutralize = null }: Props = $props();

	const columns = $derived(
		neutralize
			? [
					{ key: 'override', label: m.filters_flows_strength_override(), preview },
					{ key: 'neutralize', label: m.filters_flows_strength_neutralize(), preview: neutralize }
				]
			: [{ key: 'change', label: m.filters_flows_summary_title(), preview }]
	);
</script>

<PreviewTable caption={m.filters_flows_summary_title()} {columns} testid="impact" />
