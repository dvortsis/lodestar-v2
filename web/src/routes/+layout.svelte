<script lang="ts">
	import '../app.css';
	import Shell from '$lib/shell/Shell.svelte';

	let { children } = $props();
</script>

<Shell>{@render children()}</Shell>
