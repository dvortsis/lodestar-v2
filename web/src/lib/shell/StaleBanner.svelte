<script lang="ts">
	// Shown when the server runs another UI build than this tab (stale.ts). Quiet and polite: it
	// never takes focus and never reloads by itself. Reload goes through the leave guards, so a
	// Settings draft still gets the browser's own question.
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { hasUnsaved } from './stale';

	let unsaved = $state(hasUnsaved());
	// A draft can start after the banner shows; edits are the only way it does.
	$effect(() => {
		const ask = () => (unsaved = hasUnsaved());
		document.addEventListener('input', ask, true);
		document.addEventListener('change', ask, true);
		return () => {
			document.removeEventListener('input', ask, true);
			document.removeEventListener('change', ask, true);
		};
	});
</script>

<div class="m-4 mb-0" data-testid="stale-banner">
	<Banner>
		{m.shell_stale_text()}
		{#if unsaved}{m.shell_stale_unsaved()}{/if}
		{#snippet action()}
			<Button dense onclick={() => location.reload()}>{m.shell_stale_reload()}</Button>
		{/snippet}
	</Banner>
</div>
