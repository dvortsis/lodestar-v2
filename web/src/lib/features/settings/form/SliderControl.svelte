<script lang="ts">
	// A bounded number with `x-widget: slider` (§10.10.4): the slider and the number beside it.
	// Loaded per type by `controls.svelte.ts`.
	import Slider from '$lib/ui/fields/Slider.svelte';
	import type { LazyControlProps } from './controls.svelte';
	import NumberControl from './NumberControl.svelte';

	let { form, key, controlProps, dense = false }: LazyControlProps = $props();

	const f = $derived(form.field(key));
</script>

<div class="flex items-center gap-3">
	<Slider
		value={typeof f.value === 'number' ? f.value : (f.spec.minimum ?? 0)}
		min={f.spec.minimum}
		max={f.spec.maximum}
		disabled={f.locked}
		aria-labelledby={controlProps['aria-labelledby']}
		oninput={(e) => form.set(key, Number(e.currentTarget.value))}
		onblur={() => form.blur(key)}
	/>
	<NumberControl {form} {key} {controlProps} {dense} />
</div>
