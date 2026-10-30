<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';
</script>

<svelte:head><title>{m.settings_shell_title()} · Lodestar</title></svelte:head>
<h1 class="text-title font-semibold">{m.settings_shell_overview()}</h1>
<p>{m.settings_shell_overview_purpose()}</p>
<p class="text-muted-text">{m.settings_shell_overview_unknown()}</p>
