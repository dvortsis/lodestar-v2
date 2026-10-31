<script lang="ts">
	// The `language` widget: Automatic (follow the browser) and the UI languages Lodestar ships.
	import { m } from '$lib/paraglide/messages.js';
	import Select from '$lib/ui/fields/Select.svelte';
	import type { WidgetProps } from '../form/widgets';

	let { form, key, controlProps, dense = false }: WidgetProps = $props();
	const f = $derived(form.field(key));
	const LANGUAGES = $derived([
		{ value: '', label: m.setting_automatic() },
		{ value: 'en', label: 'English' },
		{ value: 'zh', label: '中文' }
	]);
	// A tag saved some other way (`zh-CN`) stays selectable rather than silently changing.
	const options = $derived(
		typeof f.value === 'string' && f.value && !LANGUAGES.some((l) => l.value === f.value)
			? [...LANGUAGES, { value: f.value, label: f.value }]
			: LANGUAGES
	);
</script>

<Select
	{...controlProps}
	{dense}
	{options}
	value={typeof f.value === 'string' ? f.value : ''}
	disabled={f.locked}
	onchange={(e) => form.set(key, e.currentTarget.value)}
	onblur={() => form.blur(key)}
/>
