<script lang="ts">
	import { page } from '$app/state';
	import ScreenHost from '$lib/features/settings/shell/ScreenHost.svelte';
</script>

{#key page.params.section}
	<ScreenHost id={page.params.section!} />
{/key}
