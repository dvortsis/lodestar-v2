<script lang="ts">
	// `integrations.default_client`: a select over the clients' ids, as the form has them now
	// (staged additions included). Empty means the first client.
	import { m } from '$lib/paraglide/messages.js';
	import Select from '$lib/ui/fields/Select.svelte';
	import type { WidgetProps } from '../form/widgets';
	import { toClients } from './clients';

	let { form, key, controlProps }: WidgetProps = $props();
	const f = $derived(form.field(key));
	const clients = $derived(toClients(form.field('integrations.clients').value));
	const value = $derived(typeof f.value === 'string' ? f.value : '');
	// A value naming a client that's gone still shows, so it isn't silently replaced.
	const options = $derived([
		{ value: '', label: m.settings_screen_default_first() },
		...clients.map((c) => ({ value: c.id, label: c.name || c.id })),
		...(value && !clients.some((c) => c.id === value) ? [{ value, label: value }] : [])
	]);
</script>

<Select
	{...controlProps}
	{options}
	{value}
	disabled={f.locked}
	onchange={(e) => form.set(key, e.currentTarget.value)}
	onblur={() => form.blur(key)}
/>
