<script lang="ts">
	// What the shell shows when the server is not answering. `unreachable`: nothing has answered since
	// the page opened, so a full-page state replaces the page. `reconnecting`: it answered before and
	// went quiet; a plain text banner sits above the page, which stays as it was. Fetched together
	// with the stream (see Chrome.svelte), so it is already here when the server goes away: these
	// states can only occur once the stream is running.
	import { m } from '$lib/paraglide/messages.js';
	import StateScreen from './StateScreen.svelte';

	// `offline`: the page itself could not load because nothing answered (+error.svelte). It retries on
	// its own; `status` is the live line saying when, and Try again asks now.
	let {
		kind,
		status = '',
		onretry
	}: {
		kind: 'unreachable' | 'reconnecting' | 'offline';
		status?: string;
		onretry?: () => void;
	} = $props();
</script>

{#if kind === 'offline'}
	<StateScreen
		title={m.shell_state_offline_title()}
		text={m.shell_state_offline_text()}
		scene="connection_error"
	>
		{#snippet actions()}
			<button type="button" class="btn btn-primary" onclick={() => onretry?.()}
				>{m.shell_state_retry()}</button
			>
			<p
				role="status"
				aria-live="polite"
				data-testid="offline-status"
				class="flex items-center text-small text-muted-text"
			>
				{status}
			</p>
		{/snippet}
	</StateScreen>
{:else if kind === 'unreachable'}
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
