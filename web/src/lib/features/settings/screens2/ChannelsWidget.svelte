<script lang="ts">
	// A list of choices from a fixed set (`digest_channels`, the notification routes): one checkbox
	// each, standing in for the JSON fallback the schema's enum array would get. The options are
	// the schema's own, in its order.
	import { m } from '$lib/paraglide/messages.js';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import type { WidgetProps } from '../form/widgets';
	import { channelLabel, CHOICES } from './channels';

	let { form, key, controlProps }: WidgetProps = $props();
	const f = $derived(form.field(key));
	const options = $derived(CHOICES[f.spec.name] ?? []);
	const chosen = $derived(Array.isArray(f.value) ? (f.value as unknown[]) : []);
	function toggle(option: string, on: boolean) {
		const next = options.filter((o) => (o === option ? on : chosen.includes(o)));
		form.set(key, next);
	}
</script>

<div
	role="group"
	aria-labelledby={controlProps['aria-labelledby']}
	aria-describedby={controlProps['aria-describedby']}
	class="flex flex-wrap gap-x-6 gap-y-1"
>
	{#each options as option (option)}
		<Checkbox
			label={channelLabel(option)}
			checked={chosen.includes(option)}
			disabled={f.locked}
			onchange={(e) => toggle(option, e.currentTarget.checked)}
		/>
	{:else}
		<p class="text-muted-text">{m.settings_screen_channels_none()}</p>
	{/each}
</div>
