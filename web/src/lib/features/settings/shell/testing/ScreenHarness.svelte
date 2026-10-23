<script lang="ts">
	// Tests only: the Settings layout's context around one screen (or the whole shell).
	import ScreenHost from '../ScreenHost.svelte';
	import SettingsShell from '../SettingsShell.svelte';
	import Overview from '../Overview.svelte';

	let { id }: { id?: string } = $props();
</script>

<!-- The app's <main> is where the heading is looked for. -->
<main>
	<SettingsShell>
		{#if id}<ScreenHost {id} />{:else}<Overview />{/if}
	</SettingsShell>
</main>
