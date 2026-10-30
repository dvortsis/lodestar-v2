<script lang="ts">
	import type { Snippet } from 'svelte';
	import SettingsShell from '$lib/features/settings/shell/SettingsShell.svelte';
	let { children }: { children: Snippet } = $props();
</script>

<SettingsShell>{@render children()}</SettingsShell>
