<script lang="ts">
	// General → Danger zone → Reset all settings (§10.10.7, destructive). It runs on its own, not
	// as part of Save: it counts what is saved in the app, asks with Cancel focused and the button
	// naming the action, then removes it all in one change. It cannot be undone, and says so.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { onDestroy } from 'svelte';
	import { ask } from '../form/dialogs';
	import { useSettings } from '../shell/state.svelte';

	const settings = useSettings();
	let busy = $state(false);
	const asking = new AbortController();
	onDestroy(() => asking.abort());

	async function resetAll() {
		if (!settings.index) return;
		busy = true;
		try {
			const { savedSettings, resetAll: run } = await import('./reset');
			const saved = await savedSettings(settings.index, asking.signal);
			if (saved.count === 0) {
				toast.push({ kind: 'info', text: m.settings_ui_reset_none() });
				return;
			}
			const count = formatCount(saved.count, currentLocale());
			const answer = await ask(
				{
					title: m.settings_ui_reset_confirm_title(),
					body: [m.settings_ui_reset_confirm_body({ count })],
					actions: [
						{ id: 'cancel', label: m.settings_form_cancel() },
						{ id: 'reset', label: m.settings_ui_reset_confirm({ count }), variant: 'danger' }
					],
					initial: 'cancel'
				},
				asking.signal
			);
			if (answer !== 'reset') return;
			await run(saved);
			toast.push({ kind: 'success', text: m.settings_ui_reset_done({ count }) });
			await settings.refreshSystem();
		} catch {
			toast.push({ kind: 'error', text: m.settings_ui_reset_failed() });
		} finally {
			busy = false;
		}
	}
</script>

<section class="danger" aria-labelledby="danger-title" data-testid="settings-danger">
	<h2 id="danger-title" class="text-title font-semibold">{m.settings_ui_danger_title()}</h2>
	<h3 class="text-body-lg font-semibold">{m.settings_ui_reset_title()}</h3>
	<p>{m.settings_ui_reset_help()}</p>
	<div>
		<Button
			variant="danger"
			loading={busy}
			onclick={() => void resetAll()}
			data-testid="settings-reset-all">{m.settings_ui_reset_button()}</Button
		>
	</div>
</section>

<style>
	.danger {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 3);
		padding: calc(var(--spacing) * 4);
		border: var(--border) solid var(--color-error);
		border-radius: var(--radius-box);
	}
</style>
