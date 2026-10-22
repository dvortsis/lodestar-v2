<script lang="ts">
	// What the shell shows when the server is not answering. `unreachable`: it never answered since
	// the page opened, so a full-page state replaces the page. `reconnecting`: it answered before;
	// a banner sits above the page, which stays as it was. Loaded only when needed.
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import StateScreen from './StateScreen.svelte';

	let { kind }: { kind: 'unreachable' | 'reconnecting' } = $props();
</script>

{#if kind === 'unreachable'}
	<StateScreen
		title={m.shell_state_unreachable_title()}
		text={m.shell_state_unreachable_text()}
		scene="connection_error"
	>
		{#snippet actions()}
			<button type="button" class="btn btn-primary" onclick={() => location.reload()}
				>{m.shell_state_retry()}</button
			>
		{/snippet}
	</StateScreen>
{:else}
	<div class="p-4 pb-0" data-testid="reconnecting-banner">
		<Banner kind="warning">{m.shell_banner_reconnecting()}</Banner>
	</div>
{/if}
