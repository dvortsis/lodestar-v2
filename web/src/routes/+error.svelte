<script lang="ts">
	// Kit loads this node with every route (it is the error boundary), so it stays small: the full
	// screen (illustrations included) is fetched only when something has failed. If that fetch fails
	// too (the usual reason to be here is that the server has gone), plain markup stands in for it.
	// It has messages of its own (shell_fallback_*): sharing the screen's would put them in a chunk of
	// their own, which is two more requests on every first load.
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import { serverAnswers, watchServer, type Watch } from '$lib/shell/reach';

	let View = $state<typeof import('$lib/shell/ErrorView.svelte').default>();
	let failed = $state(false);
	void import('$lib/shell/ErrorView.svelte').then(
		(mod) => (View = mod.default),
		() => (failed = true)
	);

	const notFound = $derived(page.status === 404);

	// A 5xx may be the server failing, or nothing answering at all (the route's code or data could not
	// be fetched). Ask the server: only when the request itself fails is this a connection problem.
	// Then retry with backoff, and when it answers load the page that was asked for, at the same URL.
	// A server that answers keeps the error view, so a real failure can't loop.
	let offline = $state(false);
	let line = $state('');
	let Connection = $state<typeof import('$lib/shell/ConnectionView.svelte').default>();
	let watch: Watch | undefined;

	onMount(() => {
		let gone = false;
		if (page.status >= 500) {
			void serverAnswers().then((up) => {
				if (gone || up) return;
				offline = true;
				void import('$lib/shell/ConnectionView.svelte').then(
					(mod) => (Connection = mod.default),
					() => {} // the plain markup below stands in
				);
				watch = watchServer({
					probe: () => serverAnswers(),
					onUp: () => {
						offline = false;
						// A route whose code failed to import stays failed for this page's lifetime, so a
						// client-side retry (goto) lands on the same error. One reload, at the same URL, only
						// once the server has answered: it cannot loop while it is down.
						location.reload();
					},
					onState: (state) =>
						(line =
							state.phase === 'checking'
								? m.shell_state_offline_checking()
								: m.shell_state_offline_waiting({ seconds: String(state.delayMs / 1000) }))
				});
			});
		}
		return () => {
			gone = true;
			watch?.stop();
		};
	});
</script>

{#if offline && Connection}
	<Connection kind="offline" status={line} onretry={() => watch?.retryNow()} />
{:else if offline}
	<section class="mx-auto flex w-full max-w-5xl flex-col gap-4 p-6" data-testid="state-screen">
		<h1 class="text-display font-semibold">{m.shell_fallback_offline_title()}</h1>
		<p class="text-body text-muted-text">{m.shell_fallback_offline_text()}</p>
		<div class="flex flex-wrap items-center gap-2">
			<button type="button" class="btn btn-primary" onclick={() => watch?.retryNow()}
				>{m.shell_fallback_retry()}</button
			>
			<p
				role="status"
				aria-live="polite"
				data-testid="offline-status"
				class="text-small text-muted-text"
			>
				{line}
			</p>
		</div>
	</section>
{:else if View}
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
