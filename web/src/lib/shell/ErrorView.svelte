<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import StateScreen from '$lib/shell/StateScreen.svelte';

	const notFound = $derived(page.status === 404);
</script>

<svelte:head
	><title>{notFound ? m.shell_state_not_found_title() : m.shell_state_error_title()}</title
	></svelte:head
>

<StateScreen
	title={notFound ? m.shell_state_not_found_title() : m.shell_state_error_title()}
	text={notFound ? m.shell_state_not_found_text() : m.shell_state_error_text()}
	code={notFound ? undefined : m.shell_state_error_code({ status: String(page.status) })}
	scene={notFound ? 'empty_state_explore' : undefined}
>
	{#snippet actions()}
		<a href={resolve('/search')} class="btn btn-primary">{m.shell_state_home()}</a>
		{#if !notFound}
			<button
				type="button"
				class="btn btn-outline border-field-border"
				onclick={() => location.reload()}>{m.shell_state_retry()}</button
			>
		{/if}
	{/snippet}
</StateScreen>
