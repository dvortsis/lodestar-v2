<script lang="ts">
	// The screen couldn't load its fields: the shell's usual banner with Retry.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';

	let { onretry }: { onretry: () => void } = $props();
</script>

<Banner kind="error"
	>{m.settings_form_load_error()}{#snippet action()}<Button dense onclick={onretry}
			>{m.settings_form_retry()}</Button
		>{/snippet}</Banner
>
