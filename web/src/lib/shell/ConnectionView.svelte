<script lang="ts">
	// What the shell shows when the server is not answering. `unreachable`: nothing has answered since
	// the page opened, so a full-page state replaces the page. `reconnecting`: it answered before and
	// went quiet; a plain text banner sits above the page, which stays as it was. Fetched together
	// with the stream (see Chrome.svelte), so it is already here when the server goes away: these
	// states can only occur once the stream is running.
	import { m } from '$lib/paraglide/messages.js';
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
	<p
		role="status"
		data-testid="reconnecting-banner"
		class="m-4 mb-0 rounded-box border border-base-300 bg-base-100 p-3 text-body"
	>
		{m.shell_banner_reconnecting()}
	</p>
{/if}
