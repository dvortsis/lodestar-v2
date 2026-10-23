<script lang="ts">
	// On every Settings page while saved settings wait for a restart (§10.10.6): how many, Review
	// (each field, its screen and the saved value) and Restart now. Where Lodestar can't restart
	// itself, it says how to instead, by how it is installed.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { type Component } from 'svelte';
	import { restartNow } from './restart-flow';
	import type { SystemInfo } from './system';

	let { system }: { system: SystemInfo } = $props();

	const count = $derived(system.restart_required.length);
	const how = $derived.by(() => {
		switch (system.deployment.kind) {
			case 'docker':
			case 'unraid':
				return m.settings_ui_restart_how_container();
			case 'windows_service':
				return m.settings_ui_restart_how_windows_service();
			case 'windows_portable':
				return m.settings_ui_restart_how_portable();
			default:
				return system.restart.hint ?? m.settings_ui_restart_how_other();
		}
	});

	let Review = $state.raw<Component<{ system: SystemInfo }>>();
	let reviewing = $state(false);
	function toggle() {
		reviewing = !reviewing;
		if (reviewing && !Review)
			void import('./RestartReview.svelte').then((mod) => (Review = mod.default));
	}
	let busy = $state(false);
	async function restart() {
		busy = true;
		try {
			await restartNow(system);
		} finally {
			busy = false;
		}
	}
</script>

<div id="settings-restart-banner" data-testid="settings-restart-banner" tabindex="-1">
	<Banner kind="warning">
		<span data-testid="settings-restart-count"
			>{count === 1
				? m.settings_ui_restart_banner_one()
				: m.settings_ui_restart_banner({ count: formatCount(count, currentLocale()) })}</span
		>
		{#if !system.restart.available}<span data-testid="settings-restart-how"> {how}</span>{/if}
		{#snippet action()}
			<div class="flex flex-wrap items-center gap-2">
				<Button dense onclick={toggle} aria-expanded={reviewing}
					>{reviewing
						? m.settings_ui_restart_review_hide()
						: m.settings_ui_restart_review()}</Button
				>
				{#if system.restart.available}
					<Button
						dense
						variant="primary"
						loading={busy}
						onclick={() => void restart()}
						data-testid="settings-restart-now">{m.settings_ui_restart_now()}</Button
					>
				{/if}
			</div>
		{/snippet}
	</Banner>
	{#if reviewing && Review}<Review {system} />{/if}
</div>
