<script lang="ts">
	// The restart and address banners and the restarting state: one lazily loaded chunk, brought in
	// by the layout only when there is something to show.
	import AddressBanner from './AddressBanner.svelte';
	import { flow } from './flow.svelte';
	import RestartBanner from './RestartBanner.svelte';
	import RestartingScreen from './RestartingScreen.svelte';
	import { useSettings } from './state.svelte';

	const settings = useSettings();
	const system = $derived(settings.system);
	const change = $derived(system?.address_change ?? null);
</script>

{#if system}
	{#if system.restart_required.length > 0}<RestartBanner {system} />{/if}
	{#if change?.state === 'awaiting_confirmation'}
		<AddressBanner {system} {change} refresh={settings.refreshSystem} />
	{/if}
{/if}
{#if flow.phase !== 'idle'}<RestartingScreen />{/if}
