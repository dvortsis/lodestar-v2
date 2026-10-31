<script lang="ts">
	// Per-component levels (§12.1, under Advanced): each component records at the Verbosity's level
	// unless it has its own. Libraries take the Libraries filter instead (Expert).
	import { m } from '$lib/paraglide/messages.js';
	import Select from '$lib/ui/fields/Select.svelte';
	import type { WidgetProps } from '../form/widgets';
	import { COMPONENT_IDS, componentLabel } from './event-labels.svelte';
	import { levelLabel } from './labels';
	import { LEVELS } from './rules';

	let { form, key, controlProps }: WidgetProps = $props();
	const f = $derived(form.field(key));
	const levels = $derived((f.value ?? {}) as Record<string, string>);
	const options = [
		{ value: '', label: m.settings_logging_component_default() },
		...LEVELS.map((l) => ({ value: l, label: levelLabel(l) }))
	];

	function set(component: string, level: string) {
		const next = { ...levels };
		if (level) next[component] = level;
		else delete next[component];
		form.set(key, next);
	}
</script>

<div
	role="group"
	aria-labelledby={controlProps['aria-labelledby']}
	aria-describedby={controlProps['aria-describedby']}
	class="grid gap-3 sm:grid-cols-2"
>
	{#each COMPONENT_IDS.filter((c) => c !== 'libraries') as c (c)}
		<label class="flex flex-col gap-1">
			<span class="text-label">{componentLabel(c)}</span>
			<Select
				dense
				{options}
				value={levels[c] ?? ''}
				disabled={f.locked}
				onchange={(e) => set(c, e.currentTarget.value)}
			/>
		</label>
	{/each}
</div>
