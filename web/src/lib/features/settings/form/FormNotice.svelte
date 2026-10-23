<script lang="ts">
	// The banner after a save that couldn't reach the server, or that it refused with an error
	// (§10.10.5), with Retry. Part of the edit chunk, which every save has loaded.
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { SettingsForm } from './form.svelte';

	let { form }: { form: SettingsForm } = $props();

	const retrySeconds = $derived(form.saveError?.retryAfter);
</script>

{#if form.status === 'offline'}
	<Banner kind="warning">
		<span data-testid="offline-text"
			>{retrySeconds
				? m.settings_form_busy({ seconds: formatCount(retrySeconds, currentLocale()) })
				: m.settings_form_offline()}</span
		>
		{#snippet action()}
			<Button dense onclick={() => void form.retry()}>{m.settings_form_retry()}</Button>
		{/snippet}
	</Banner>
{:else if form.status === 'save-error'}
	<Banner kind="error">
		{m.settings_form_error_server()}
		{#snippet action()}
			<Button dense onclick={() => void form.retry()}>{m.settings_form_retry()}</Button>
		{/snippet}
	</Banner>
{/if}
