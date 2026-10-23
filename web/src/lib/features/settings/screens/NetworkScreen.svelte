<script lang="ts">
	// Network & VPN (§10.10.8): the generated fields, the lockout dialog as this screen's own
	// disruptive gate, and the reachability panel beside the DHT group. Egress, the kill switch and
	// the VPN parts have no schema fields yet, so nothing shows for them.
	import { onDestroy, type Component } from 'svelte';
	import type { SettingsForm } from '../form';
	import type { ResolvedScreen } from '../shell/registry';
	import GeneratedScreen from './GeneratedScreen.svelte';

	let { resolved }: { resolved: ResolvedScreen } = $props();

	// The panel and its refresh load when the screen opens, not with the route.
	let Panel = $state.raw<Component>();
	void import('./ReachabilityPanel.svelte').then((mod) => (Panel = mod.default));

	const asking = new AbortController();
	onDestroy(() => asking.abort());
	// The dialog's text and code load when a disruptive change is saved, not with the screen.
	const oncreate = (form: SettingsForm) =>
		form.gate('disruptive', async (c) => {
			const { lockoutGate } = await import('./lockout');
			return lockoutGate(asking.signal)(c);
		});
</script>

<GeneratedScreen {resolved} {oncreate}>
	{#snippet afterGroup(group)}
		{#if group === 'dht' && Panel}<Panel />{/if}
	{/snippet}
</GeneratedScreen>
