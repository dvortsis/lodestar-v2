<script lang="ts">
	// The offer of a draft from earlier (§10.10.5): Restore or Discard.
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { SettingsForm } from './form.svelte';

	let { form }: { form: SettingsForm } = $props();
</script>

{#if form.draft}
	<Banner kind="info" title={m.settings_form_draft_title()}>
		<span data-testid="draft-text"
			>{form.draft.dropped
				? m.settings_form_draft_dropped({
						count: formatCount(form.draft.dropped, currentLocale())
					})
				: m.settings_form_draft_body()}</span
		>
		{#snippet action()}
			<div class="flex gap-2">
				<Button dense onclick={() => form.dropDraft()}>{m.settings_form_discard()}</Button>
				<Button dense variant="primary" onclick={() => form.restoreDraft()}
					>{m.settings_form_restore()}</Button
				>
			</div>
		{/snippet}
	</Banner>
{/if}
