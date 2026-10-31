<script lang="ts">
	// The privacy banners (§10.10.12): one while any identifying kind is on, and one for each opt-in,
	// each with the time left and Turn off now. They show what `GET /logs/catalog` says, which is
	// reloaded after every change and once a deadline passes, so they follow the server's state.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import type { SettingsForm } from '../form';
	import type { LoggingCatalog } from './catalog.svelte';
	import { identifyingOn, optInOn, soonest, timeLeft } from './state';

	let { catalog, form }: { catalog: LoggingCatalog; form?: SettingsForm } = $props();

	const c = $derived(catalog.catalog);
	const banners = $derived(
		c
			? [
					{
						id: 'identifying',
						title: m.settings_logging_banner_identifying(),
						left: timeLeft(soonest(identifyingOn(c, catalog.now)), catalog.now)
					},
					{
						id: 'peer_activity',
						title: m.settings_logging_banner_peer(),
						left: timeLeft(optInOn(c, 'peer_activity', catalog.now)?.until ?? 0, catalog.now)
					},
					{
						id: 'filter_changes',
						title: m.settings_logging_banner_filters(),
						left: timeLeft(optInOn(c, 'filter_changes', catalog.now)?.until ?? 0, catalog.now)
					}
				].filter((b) => b.left)
			: []
	);

	let busy = $state(false);
	let error = $state('');
	async function turnOff(id: string) {
		busy = true;
		error = '';
		try {
			const p = await import('./privacy');
			const ctx = { catalog, form };
			if (id === 'identifying') await p.turnOffIdentifying(ctx);
			else await p.setOptIn(ctx, id as 'peer_activity' | 'filter_changes', false);
		} catch (e) {
			error = m.settings_logging_failed_off({ message: (e as Error).message });
		} finally {
			busy = false;
		}
	}
</script>

{#each banners as b (b.id)}
	<Banner kind="warning" title={b.title}>
		<span data-testid="logging-banner-{b.id}">{b.left}</span>
		{#snippet action()}
			<Button variant="secondary" disabled={busy} onclick={() => turnOff(b.id)}
				>{m.settings_logging_turn_off()}</Button
			>
		{/snippet}
	</Banner>
{/each}
{#if error}<p role="alert" class="text-small text-error">{error}</p>{/if}
