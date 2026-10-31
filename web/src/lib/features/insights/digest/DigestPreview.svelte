<script lang="ts">
	// The digest preview (§10.9.7) on Settings → Insights & digest. It reads the *saved* settings, so
	// it is loaded on a button press, not at mount: no request and no work until someone asks, and the
	// saved "digest" value decides the off state. The plain-text version is rendered as text in a
	// `pre`; the HTML version is never shown (nothing here can inject markup).
	import { ApiError, api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';

	type Phase = 'idle' | 'loading' | 'off' | 'ready' | 'error';
	let phase = $state<Phase>('idle');
	let subject = $state('');
	let text = $state('');
	let error = $state('');

	async function load() {
		phase = 'loading';
		try {
			const saved = await api.get('/settings/{section}', { path: { section: 'insights' } });
			const mode = saved.fields.digest?.value;
			if (mode !== 'daily' && mode !== 'weekly') {
				phase = 'off';
				return;
			}
			const res = await api.get('/digest/preview', { query: { period: mode } });
			subject = res.subject;
			text = res.text;
			phase = 'ready';
		} catch (e) {
			error = e instanceof ApiError && e.message ? e.message : String(e);
			phase = 'error';
		}
	}
</script>

<div class="flex flex-col gap-3" data-testid="digest-preview">
	<h3 class="text-label font-semibold">{m.insights_digest_preview_title()}</h3>
	<p class="text-muted-text">{m.insights_digest_preview_help()}</p>
	<div>
		<Button
			loading={phase === 'loading'}
			onclick={() => void load()}
			data-testid="digest-preview-load"
			>{phase === 'ready' || phase === 'error' || phase === 'off'
				? m.insights_digest_preview_refresh()
				: m.insights_digest_preview_button()}</Button
		>
	</div>
	<div role="status" aria-live="polite" class="flex flex-col gap-2">
		{#if phase === 'loading'}
			<p class="text-muted-text">{m.insights_digest_preview_loading()}</p>
		{:else if phase === 'off'}
			<p data-testid="digest-preview-off">{m.insights_digest_preview_off()}</p>
		{:else if phase === 'error'}
			<p class="text-error" data-testid="digest-preview-error">
				{m.insights_digest_preview_error({ message: error })}
			</p>
		{:else if phase === 'ready'}
			<p class="font-semibold" data-testid="digest-preview-subject">
				{m.insights_digest_preview_subject({ subject })}
			</p>
		{/if}
	</div>
	{#if phase === 'ready'}
		<!-- Focusable so the scrolling block works from the keyboard. -->
		<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
		<pre
			class="digest-text overflow-auto rounded-box border border-base-300 bg-base-200 p-4 font-mono text-small whitespace-pre-wrap"
			tabindex="0"
			aria-label={m.insights_digest_preview_label()}
			data-testid="digest-preview-text">{text}</pre>
	{/if}
</div>

<style>
	.digest-text {
		max-height: 50dvh;
	}
</style>
