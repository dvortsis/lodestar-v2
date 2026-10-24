<script lang="ts">
	// The sections of one torrent (§10.5), shared by the drawer and the /t/{hash} page. The title and
	// the primary action live in the container's header (the Drawer's, or the page's h1). Everything
	// below the fold (Contents, Parsed) and the error states load as separate chunks when needed, to
	// keep the first load inside the 15 KB budget (§10.1).
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { showBidi } from '$lib/util/bidi';
	import { isMagnet } from '$lib/util/magnet';
	import { whenVisible } from './visible';
	import Summary from './summary/Summary.svelte';
	import { resolvedVia } from './parsed/labels';
	import type { DetailModel } from './model.svelte';
	import './detail.css';

	interface Props {
		model: DetailModel;
		mode: 'drawer' | 'page';
		/** "Back to Search" in the not-found state (the drawer closes; the page links). */
		onBack?: () => void;
	}
	let { model, mode, onBack }: Props = $props();

	// The page's title is its h1, so its sections are h2; the drawer's title is an h2.
	const level = $derived(mode === 'page' ? 2 : 3);
	const detail = $derived(model.detail);
	const hit = $derived(model.hit);
	const error = $derived(model.error);
	const rawName = $derived(detail?.name ?? hit?.name ?? '');
	const magnet = $derived(detail && isMagnet(detail.magnet) ? detail.magnet : null);

	// Lazy chunks, held as components once loaded (no {#await}: its runtime would ship with Search).
	let Contents = $state.raw<typeof import('./Contents.svelte').default>();
	let Parsed = $state.raw<typeof import('./parsed/Parsed.svelte').default>();
	let ErrorState = $state.raw<typeof import('./ErrorState.svelte').default>();
	let contentsAsked = false;
	let parsedAsked = false;
	function loadContents() {
		if (contentsAsked) return;
		contentsAsked = true;
		void import('./Contents.svelte').then((mod) => (Contents = mod.default));
	}
	function loadParsed() {
		if (parsedAsked) return;
		parsedAsked = true;
		void import('./parsed/Parsed.svelte').then((mod) => (Parsed = mod.default));
	}
	$effect(() => {
		if (error && !ErrorState)
			void import('./ErrorState.svelte').then((mod) => (ErrorState = mod.default));
	});
</script>

<div class="ls-detail" data-testid="detail-view" data-mode={mode}>
	{#if hit?.title && rawName && hit.title !== rawName}
		<div class="ls-detail-stack">
			<span class="text-small text-muted-text">{m.raw_name()}</span>
			<Mono wrap><span data-testid="detail-raw-name">{showBidi(rawName)}</span></Mono>
			{#if hit.resolved_via}
				<span class="text-small text-muted-text" data-testid="detail-resolved-via"
					>{m.detail_resolved_via({ method: resolvedVia(hit.resolved_via) })}</span
				>
			{/if}
		</div>
	{/if}

	{#if error && ErrorState}
		<ErrorState {error} {onBack} {level} onRetry={() => model.retry()} />
	{/if}

	{#if detail}
		<div class="ls-detail-row">
			{#if magnet}
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- a magnet URI, checked by isMagnet -->
				<a class="btn btn-ghost btn-sm" href={magnet} data-testid="detail-open-magnet"
					>{m.detail_open_magnet()}</a
				>
			{/if}
			{#if mode === 'drawer'}
				<a
					class="btn btn-ghost btn-sm"
					href={resolve('/t/[hash]', { hash: detail.hash })}
					data-testid="detail-open-full">{m.detail_open_full()}</a
				>
			{/if}
		</div>
		<Summary {detail} {hit} {level} />
		<section
			class="ls-detail-section"
			aria-labelledby="detail-h-contents"
			data-section="contents"
			use:whenVisible={loadContents}
		>
			<div role="heading" aria-level={level} id="detail-h-contents">
				{m.detail_section_contents()}
			</div>
			{#if Contents}
				<Contents {detail} {hit} />
			{:else}
				<div aria-busy="true"><Skeleton class="ls-detail-tree" /></div>
			{/if}
		</section>
		<section
			class="ls-detail-section"
			aria-labelledby="detail-h-parsed"
			data-section="parsed"
			use:whenVisible={loadParsed}
		>
			<div role="heading" aria-level={level} id="detail-h-parsed">{m.detail_section_parsed()}</div>
			{#if Parsed}
				<Parsed hash={detail.hash} {hit} />
			{:else}
				<Skeleton lines={3} />
			{/if}
		</section>
		<!-- Slots for the Swarm, Related and Notes & labels sections (follow-ups), in story order. -->
	{:else if !error}
		<div aria-busy="true" aria-label={m.detail_loading()}>
			<Skeleton lines={6} />
		</div>
	{/if}
</div>
