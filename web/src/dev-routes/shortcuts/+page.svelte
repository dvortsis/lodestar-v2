<script lang="ts">
	// Test-only page for the keyboard screen (S-keys): the screen itself, plus the one command a
	// results list would register (`results.next`), so a remap can be seen to take effect.
	import { onMount } from 'svelte';
	import { keys } from '$lib/keys';
	import KeyboardScreen from '$lib/features/shortcuts/KeyboardScreen.svelte';

	let fired = $state(0);

	onMount(() =>
		keys.register({
			id: 'results.next',
			title: 'Next result',
			section: 'Results',
			run: () => fired++
		})
	);
</script>

<KeyboardScreen />
<p class="p-4" data-testid="fired">{fired}</p>
