<script lang="ts">
	// A screen's navigation icon: `icon` at once, else the gear, which `iconLoad` replaces when the
	// icon has loaded (a failed load keeps the gear).
	import type { Component } from 'svelte';
	import IconGear from '~icons/ph/gear';
	import type { Screen } from './screens';

	let { screen }: { screen: Screen } = $props();
	let loaded = $state<Component>();
	$effect(() => {
		const load = screen.iconLoad;
		if (!load || screen.icon) return;
		let live = true;
		void load().then(
			(mod) => {
				if (live) loaded = mod.default;
			},
			() => {}
		);
		return () => {
			live = false;
		};
	});
	const Icon = $derived(screen.icon ?? loaded ?? IconGear);
</script>

<Icon class="size-5 shrink-0" aria-hidden="true" />
