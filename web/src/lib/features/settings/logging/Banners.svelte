<script lang="ts">
	// The privacy banners (§10.10.12): one while any identifying kind is on, and one for each opt-in,
	// each with the time left and Turn off now. They show what `GET /logs/catalog` says, which is
	// reloaded after every change and once a deadline passes, so they follow the server's state.
	import { onDestroy } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import type { SettingsForm } from '../form';
	import type { LoggingCatalog } from './catalog.svelte';
	import { identifyingOn, optInOn, endOf, timeLeft } from './state';

	let { catalog, form }: { catalog: LoggingCatalog; form?: SettingsForm } = $props();

	const c = $derived(catalog.catalog);
	const banners = $derived(
		c
			? [
					{
						id: 'identifying',
						title: m.settings_logging_banner_identifying(),
						left: timeLeft(endOf(identifyingOn(c, catalog.now)), catalog.now)
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

	const leaving = new AbortController();
	onDestroy(() => leaving.abort());

	let busy = $state(false);
	let error = $state('');
	async function turnOff(id: string) {
		busy = true;
		error = '';
		try {
			const p = await import('./privacy');
			const ctx = { catalog, form, signal: leaving.signal };
			if (id === 'identifying') await p.turnOffIdentifying(ctx);
			else await p.setOptIn(ctx, id as 'peer_activity' | 'filter_changes', false);
		} catch (e) {
			error = m.settings_logging_failed_off({ message: (e as Error).message });
		} finally {
			busy = false;
			// The banner and its button may be gone: keep focus on the page, not the body.
			if (!document.activeElement || document.activeElement === document.body)
				document.getElementById('logging-title')?.focus();
		}
	}
</script>

{#if !catalog.catalog && catalog.error}
	<!-- Without the server's state, the screen can't say whether torrent details are being logged. -->
	<Banner kind="warning" title={m.settings_logging_state_unknown()}>
		<span data-testid="logging-banner-unknown">{m.settings_logging_unavailable()}</span>
		{#snippet action()}
			<Button variant="secondary" onclick={() => void catalog.load()}
				>{m.settings_form_retry()}</Button
			>
		{/snippet}
	</Banner>
{/if}
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
