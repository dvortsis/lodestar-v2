<script lang="ts">
	// Settings → Insights & digest (§10.10.8): the digest's schedule and channels, query logging
	// behind its Privacy dialog (§10.10.7), retention, and Clear usage data. The digest preview
	// (§10.9.7) mounts at `data-slot="digest-preview"`, built by S-insights-3.
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import type { FieldSpec } from '../form';
	import { ask } from '../form/dialogs';
	import ChannelsWidget from './ChannelsWidget.svelte';
	import ClearUsage from './ClearUsage.svelte';
	import GroupedForm from './GroupedForm.svelte';
	import { num } from './staged';

	let { fields }: { fields: FieldSpec[] } = $props();
</script>

<GroupedForm
	id="insights"
	{fields}
	widgets={{ digest_channels: ChannelsWidget }}
	setup={(form) =>
		// Turning it off asks nothing. Turning it on says what is recorded, and Cancel keeps the save.
		form.gate('privacy', async () => {
			if (form.field('insights.query_logging').value !== true) return true;
			const days = formatCount(num(form, 'insights.query_retention_days') || 30);
			return (
				(await ask({
					title: m.settings_screen_privacy_title(),
					body: [
						m.settings_screen_privacy_what(),
						m.settings_screen_privacy_where(),
						m.settings_screen_privacy_days({ days })
					],
					actions: [
						{ id: 'cancel', label: m.settings_form_cancel() },
						{ id: 'on', label: m.settings_screen_privacy_confirm() }
					],
					initial: 'cancel'
				})) === 'on'
			);
		})}
>
	{#snippet after(group)}
		{#if group === 'digest'}
			<div data-slot="digest-preview"></div>
		{:else if group === 'usage'}
			<ClearUsage />
		{/if}
	{/snippet}
</GroupedForm>
