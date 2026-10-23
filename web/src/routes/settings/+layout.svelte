<script lang="ts">
	import SettingsShell from '$lib/features/settings/shell/SettingsShell.svelte';

	let { children } = $props();
</script>

<SettingsShell>{@render children()}</SettingsShell>
