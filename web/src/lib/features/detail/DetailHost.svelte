<script lang="ts">
	// The drawer the search page mounts (§10.5, §10.15). This shell is all the search page's lazy
	// import pulls in; the drawer itself (DetailHostBody) loads right after it, so Search's preload
	// list stays short.
	import { onMount, type Component } from 'svelte';

	let Body = $state.raw<Component>();
	onMount(() => {
		let gone = false;
		void import('./DetailHostBody.svelte').then((mod) => {
			if (!gone) Body = mod.default;
		});
		return () => (gone = true);
	});
</script>

{#if Body}<Body />{/if}
