<script lang="ts">
	import { onDestroy, tick } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { requestSettingsExport } from './export';

	let preferences = $state(false);
	let busy = $state(false);
	let status = $state<'idle' | 'started' | 'failed' | 'cancelled'>('idle');
	let root: HTMLElement;
	let controller: AbortController | undefined;
	let downloadUrl: string | undefined;
	let gone = false;
	onDestroy(() => {
		gone = true;
		controller?.abort();
		if (downloadUrl) URL.revokeObjectURL(downloadUrl);
	});

	async function download() {
		if (busy) return;
		const request = (controller = new AbortController());
		busy = true;
		status = 'idle';
		try {
			const file = await requestSettingsExport(preferences, request.signal);
			if (gone || request.signal.aborted || request !== controller) return;
			if (downloadUrl) URL.revokeObjectURL(downloadUrl);
			downloadUrl = URL.createObjectURL(file);
			const link = document.createElement('a');
			link.href = downloadUrl;
			link.download = 'lodestar-settings.toml';
			document.body.append(link);
			try {
				link.click();
			} finally {
				link.remove();
			}
			status = 'started';
		} catch {
			if (!gone && !request.signal.aborted && request === controller) status = 'failed';
		} finally {
			if (!gone && request === controller) busy = false;
		}
	}
	async function cancel() {
		controller?.abort();
		controller = undefined;
		busy = false;
		status = 'cancelled';
		await tick();
		if (!gone) root.querySelector<HTMLButtonElement>('[data-export-download]')?.focus();
	}
</script>

<section class="flex flex-col gap-4" aria-labelledby="settings-portability" bind:this={root}>
	<h2 id="settings-portability" class="text-body-lg font-semibold">
		{m.settings_shell_portability()}
	</h2>
	<p>{m.settings_shell_export_description()}</p>
	<p class="text-small text-muted-text">{m.settings_shell_export_exclusions()}</p>
	<Toggle
		label={m.settings_shell_export_preferences()}
		bind:checked={preferences}
		disabled={busy}
	/>
	<div class="flex flex-wrap gap-3">
		<Button data-export-download loading={busy} onclick={() => void download()}
			>{m.settings_shell_export_download()}</Button
		>
		{#if busy}<Button onclick={() => void cancel()}>{m.settings_form_cancel()}</Button>{/if}
	</div>
	<div role="status" aria-atomic="true">
		{#if busy}{m.settings_shell_export_loading()}
		{:else if status === 'failed'}{m.settings_shell_export_failed()}
		{:else if status === 'started'}{m.settings_shell_export_started()}
		{:else if status === 'cancelled'}{m.settings_shell_export_cancelled()}{/if}
	</div>
</section>
