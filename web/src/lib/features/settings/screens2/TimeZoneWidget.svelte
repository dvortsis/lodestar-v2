<script lang="ts">
	// The `time-zone` widget: a text input with the browser's IANA names as suggestions. The
	// server checks the name. Empty follows the browser, and the placeholder says so.
	import { m } from '$lib/paraglide/messages.js';
	import Input from '$lib/ui/fields/Input.svelte';
	import type { WidgetProps } from '../form/widgets';

	let { form, key, controlProps, dense = false }: WidgetProps = $props();
	const f = $derived(form.field(key));
	const zones: string[] = (() => {
		try {
			return Intl.supportedValuesOf('timeZone');
		} catch {
			return [];
		}
	})();
	const listId = `${controlProps.id}-zones`;
</script>

<Input
	{...controlProps}
	{dense}
	list={listId}
	placeholder={m.setting_automatic()}
	value={typeof f.value === 'string' ? f.value : ''}
	disabled={f.locked}
	spellcheck={false}
	autocomplete="off"
	oninput={(e) => form.set(key, e.currentTarget.value)}
	onblur={() => form.blur(key)}
/>
<datalist id={listId}>
	{#each zones as zone (zone)}<option value={zone}></option>{/each}
</datalist>
