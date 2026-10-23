<script lang="ts">
	// The error states of one torrent: not in the index, Adult while Adult is hidden, and a failed
	// load (network or 5xx) with Retry. A 401 is the shell's.
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import type { ApiError } from '$lib/api/client';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';

	interface Props {
		error: ApiError;
		/** The drawer closes; the page links to /search. */
		onBack?: () => void;
		onRetry: () => void;
	}
	let { error, onBack, onRetry }: Props = $props();

	const missing = $derived(error.code === 'torrent_not_found' || error.code === 'bad_hash');
	const adultHidden = $derived(error.code === 'adult_hidden');
</script>

{#if missing || adultHidden}
	<div class="ls-detail-stack" data-testid={missing ? 'detail-not-found' : 'detail-adult-hidden'}>
		<h3 class="text-title font-semibold">
			{missing ? m.detail_not_found_title() : m.detail_adult_title()}
		</h3>
		<p class="text-muted-text">{missing ? m.detail_not_found_body() : m.detail_adult_body()}</p>
		<div class="ls-detail-row">
			{#if adultHidden}
				<!-- Provisional until S-appearance owns this page. -->
				<a class="btn btn-outline border-field-border" href={resolve('/settings/appearance' as '/')}
					>{m.detail_adult_link()}</a
				>
			{/if}
			{#if onBack}
				<Button onclick={onBack}>{m.detail_back_search()}</Button>
			{:else}
				<a class="btn btn-outline border-field-border" href={resolve('/search')}
					>{m.detail_back_search()}</a
				>
			{/if}
		</div>
	</div>
{:else}
	<Banner kind="error" title={m.detail_error_title()}>
		{m.detail_error_body()}
		{#snippet action()}<Button dense onclick={onRetry}>{m.detail_retry()}</Button>{/snippet}
	</Banner>
{/if}
