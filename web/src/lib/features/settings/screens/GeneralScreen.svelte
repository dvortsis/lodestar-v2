<script lang="ts">
	// General (§10.10.8): the generated fields, then Backup & portability (`#backup`) and the
	// Danger zone. The panels are lazy; a `#backup` link scrolls to the section once it is there.
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { tick, type Component } from 'svelte';
	import type { ResolvedScreen } from '../shell/registry';
	import DangerZone from './DangerZone.svelte';
	import GeneratedScreen from './GeneratedScreen.svelte';

	let { resolved }: { resolved: ResolvedScreen } = $props();

	let Backup = $state.raw<Component>();
	void import('./BackupPanel.svelte').then((mod) => (Backup = mod.default));

	let section = $state<HTMLElement>();
	$effect(() => {
		if (page.url.hash !== '#backup' || !Backup || !section) return;
		void tick().then(() => {
			section?.scrollIntoView?.({ block: 'start' });
			section?.querySelector<HTMLElement>('h2')?.focus();
		});
	});
</script>

<GeneratedScreen {resolved}>
	{#snippet footer()}
		<section
			id="backup"
			bind:this={section}
			class="flex flex-col gap-4"
			aria-labelledby="backup-title"
			data-testid="settings-backup-section"
		>
			<h2 id="backup-title" tabindex="-1" class="text-title font-semibold outline-none">
				{m.settings_ui_backup_title()}
			</h2>
			<p>{m.settings_ui_backup_lead()}</p>
			{#if Backup}<Backup />{/if}
		</section>
		<DangerZone />
	{/snippet}
</GeneratedScreen>
