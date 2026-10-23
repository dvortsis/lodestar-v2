<script lang="ts">
	// Kit loads this node with every route (it is the error boundary), so it stays small: the full
	// screen (illustrations included) is fetched only when something has failed. If that fetch fails
	// too (the usual reason to be here is that the server has gone), plain markup stands in for it.
	// It has messages of its own (shell_fallback_*): sharing the screen's would put them in a chunk of
	// their own, which is two more requests on every first load.
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';

	let View = $state<typeof import('$lib/shell/ErrorView.svelte').default>();
	let failed = $state(false);
	void import('$lib/shell/ErrorView.svelte').then(
		(mod) => (View = mod.default),
		() => (failed = true)
	);

	const notFound = $derived(page.status === 404);
</script>

{#if View}
	<View />
{:else if failed}
	<section class="mx-auto flex w-full max-w-5xl flex-col gap-4 p-6" data-testid="state-screen">
		<h1 class="text-display font-semibold">
			{notFound ? m.shell_fallback_not_found_title() : m.shell_fallback_error_title()}
		</h1>
		{#if !notFound}
			<p class="font-mono text-small text-muted-text">
				{m.shell_fallback_error_code({ status: String(page.status) })}
			</p>
		{/if}
		<div class="flex flex-wrap gap-2">
			<a href={resolve('/search')} class="btn btn-primary">{m.shell_fallback_home()}</a>
			{#if !notFound}
				<button
					type="button"
					class="btn btn-outline border-field-border"
					onclick={() => location.reload()}>{m.shell_fallback_retry()}</button
				>
			{/if}
		</div>
	</section>
{/if}
