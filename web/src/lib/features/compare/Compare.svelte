<script lang="ts">
	// /compare?h=a,b,c (§10.3, §10.6). One POST /compare per list; the server computes every
	// difference, scale and file lists, so removing a column asks again and nothing is recomputed here.
	import { tick, untrack, type Component } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { api, ApiError, isAbort, latest } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import StateScene from '$lib/shell/StateScene.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import ErrorPanel from './ErrorPanel.svelte';
	import { copyShortcut, copyText } from './copy-magnet';
	import { compareHref, parseHashes } from './href';
	import type { CompareResponse, LayoutProps } from './model';

	// A skeleton for an answer under this long would only flash.
	const SKELETON_DELAY_MS = 300;
	// Breakpoints x-large and xx-large (design-tokens.md §7). Four or five columns need the wider one.
	const X_LARGE = '(min-width: 1024px)';
	const XX_LARGE = '(min-width: 1366px)';
	const MAX_COLUMNS_AT_X_LARGE = 3;

	const hashes = $derived(parseHashes(page.url.searchParams.get('h')));
	const key = $derived(hashes.join(','));

	// The populated view (layouts, labels, formatters, the file lists) loads beside the request.
	let Body = $state.raw<Component<LayoutProps & { columns: boolean }>>();
	const loadBody = () =>
		import('./Body.svelte')
			.then((mod) => (Body = mod.default))
			.catch(() => {
				// The chunk is gone (a newer deploy, or the connection dropped): same as a network error.
				error = new ApiError(0, 'network', 'chunk');
			});

	const slot = latest();
	let data = $state.raw<CompareResponse>();
	let error = $state.raw<ApiError>();
	let busy = $state(false);
	let showSkeleton = $state(false);
	let skeletonTimer: ReturnType<typeof setTimeout> | undefined;
	let seq = 0;

	function load(list: string[]) {
		clearTimeout(skeletonTimer);
		showSkeleton = false;
		if (list.length < 2) {
			slot.cancel();
			data = undefined;
			error = undefined;
			busy = false;
			return;
		}
		// The last good comparison stays until the next one arrives, or fails (the stale state).
		const mine = ++seq;
		busy = true;
		void loadBody();
		if (!data) skeletonTimer = setTimeout(() => (showSkeleton = true), SKELETON_DELAY_MS);
		slot
			.run((signal) => api.post('/compare', { body: { hashes: list }, signal }))
			.then((res) => {
				data = res;
				error = undefined;
			})
			.catch((e: unknown) => {
				if (isAbort(e)) return;
				error = e instanceof ApiError ? e : new ApiError(0, 'network', String(e));
			})
			.finally(() => {
				if (mine !== seq) return;
				clearTimeout(skeletonTimer);
				showSkeleton = false;
				busy = false;
			});
	}
	$effect(() => {
		const k = key;
		untrack(() => load(parseHashes(k)));
		return () => clearTimeout(skeletonTimer);
	});

	const FIXABLE = new Set(['compare_count', 'duplicate_hash', 'bad_hash']);
	const failure = $derived(
		error
			? FIXABLE.has(error.code)
				? ('fixable' as const)
				: error.status === 404 && error.code === 'torrent_not_found'
					? ('missing' as const)
					: ('transient' as const)
			: undefined
	);
	const fixableText = $derived(
		error?.code === 'compare_count'
			? m.compare_error_count()
			: error?.code === 'duplicate_hash'
				? m.compare_error_duplicate()
				: m.compare_error_bad_hash()
	);
	const stale = $derived(failure === 'transient' && data !== undefined);

	// Columns from x-large (xx-large for four or five); the attribute-first list below that.
	const matches = (q: string) => typeof matchMedia !== 'function' || matchMedia(q).matches;
	let atXLarge = $state(matches(X_LARGE));
	let atXxLarge = $state(matches(XX_LARGE));
	$effect(() => {
		if (typeof matchMedia !== 'function') return;
		const lists = [matchMedia(X_LARGE), matchMedia(XX_LARGE)];
		const sync = () => {
			atXLarge = lists[0]!.matches;
			atXxLarge = lists[1]!.matches;
		};
		sync();
		lists.forEach((l) => l.addEventListener('change', sync));
		return () => lists.forEach((l) => l.removeEventListener('change', sync));
	});
	const columns = $derived(hashes.length <= MAX_COLUMNS_AT_X_LARGE ? atXLarge : atXxLarge);

	// --- Remove: a history entry (Back restores it); focus goes to a neighbour's first action ---
	const byId = (id: string) => document.getElementById(id);
	let focusAfter: { key: string; hash: string | undefined } | undefined;
	function remove(hash: string) {
		const at = hashes.indexOf(hash);
		// Not in the URL (a stale column): nothing to remove. One copy only, if it is listed twice.
		if (at === -1) return;
		const next = hashes.filter((_, i) => i !== at);
		focusAfter = { key: next.join(','), hash: hashes[at + 1] ?? hashes[at - 1] };
		// eslint-disable-next-line svelte/no-navigation-without-resolve
		void goto(compareHref(next, page.url.searchParams), { keepFocus: true, noScroll: true });
	}
	$effect(() => {
		// Read both first: an early return would leave the effect with no dependencies.
		const settled = !busy && focusAfter?.key === key;
		if (!focusAfter || !settled) return;
		const { hash } = focusAfter;
		focusAfter = undefined;
		void tick().then(() => {
			const column = [...document.querySelectorAll<HTMLElement>('[data-hash]')].find(
				(el) => el.dataset.hash === hash
			);
			(
				column?.querySelector<HTMLElement>('[data-first-action]') ?? byId('compare-heading')
			)?.focus();
		});
	});

	// --- Copy magnet: the server's magnet, fetched on hover or focus and otherwise on press ------
	const magnets: Record<string, Promise<string>> = {};
	function magnetFor(hash: string): Promise<string> {
		let p = magnets[hash];
		if (!p) {
			p = api.get('/torrents/{hash}', { path: { hash } }).then((t) => t.magnet);
			p.catch(() => delete magnets[hash]);
			magnets[hash] = p;
		}
		return p;
	}
	const prefetch = (hash: string) => void magnetFor(hash).catch(() => {});
	let manual = $state<string>();
	async function copy(hash: string) {
		let magnet: string;
		try {
			magnet = await magnetFor(hash);
		} catch {
			toast.push({ kind: 'error', text: m.compare_copy_failed() });
			return;
		}
		if (await copyText(magnet)) {
			manual = undefined;
			toast.push({ kind: 'success', text: m.compare_copied() });
		} else {
			manual = magnet;
			void tick().then(() => {
				const field = byId('compare-manual-field') as HTMLInputElement | null;
				// Set by hand: a `value` attribute would add a runtime helper every page loads.
				if (field) field.value = magnet;
				field?.focus();
				field?.select();
			});
		}
	}
</script>

<div class="cmp-page">
	<h1 class="cmp-h1" id="compare-heading" tabindex="-1">{m.compare_title()}</h1>

	{#if manual !== undefined}
		<div class="cmp-manual" role="group" aria-label={m.compare_manual_label()}>
			<p>{m.compare_manual_help({ shortcut: copyShortcut() })}</p>
			<input
				id="compare-manual-field"
				readonly
				class="input border-field-border cmp-field font-mono {FOCUS_RING}"
				aria-label={m.compare_manual_label()}
				data-testid="compare-manual-magnet"
			/>
			<Button variant="quiet" dense onclick={() => (manual = undefined)}
				>{m.common_dismiss()}</Button
			>
		</div>
	{/if}

	<div aria-busy={busy || (data !== undefined && !Body)} data-testid="compare-region">
		{#if hashes.length < 2}
			<EmptyState
				title={m.compare_choose_title()}
				text={m.compare_choose_text()}
				data-testid="compare-choose"
			>
				{#snippet illustration()}<StateScene id="empty_state_explore" />{/snippet}
				{#snippet action()}
					<a href={resolve('/search')} class="btn btn-primary {FOCUS_RING} {TARGET_MIN}"
						>{m.compare_go_to_search()}</a
					>
				{/snippet}
			</EmptyState>
		{:else if failure === 'fixable'}
			<ErrorPanel
				testid="compare-error-{error?.code}"
				title={m.compare_error_title()}
				text={fixableText}
				{hashes}
				onRemove={remove}
			/>
		{:else if failure === 'missing'}
			<ErrorPanel
				testid="compare-error-not-found"
				title={m.compare_error_title()}
				text={m.compare_error_not_found()}
				{hashes}
				onRemove={remove}
			/>
		{:else if failure === 'transient' && (!stale || !Body)}
			<Banner kind="error" title={m.compare_error_title()}>
				{error?.code === 'network' ? m.compare_error_network() : m.compare_error_server()}
				{#snippet action()}
					<Button dense onclick={() => load(hashes)}>{m.compare_retry()}</Button>
				{/snippet}
			</Banner>
		{:else if data && Body}
			{#if stale}
				<div class="cmp-stale">
					<Banner kind="warning" title={m.compare_stale_title()}>
						{m.compare_stale_text()}
						{#snippet action()}
							<Button dense onclick={() => load(hashes)}>{m.compare_retry()}</Button>
						{/snippet}
					</Banner>
				</div>
			{/if}
			<Body {data} {columns} onCopy={copy} onPrefetch={prefetch} onRemove={remove} />
		{:else if showSkeleton || data}
			<div class="cmp-skeleton" data-testid="compare-skeleton">
				{#each Array.from({ length: 9 }, (_, n) => n) as n (n)}<Skeleton />{/each}
			</div>
		{/if}
	</div>
</div>

<style>
	.cmp-page {
		padding: calc(var(--spacing) * 4);
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
	}
	.cmp-h1 {
		font-size: var(--text-display);
		line-height: var(--text-display--line-height);
		letter-spacing: var(--text-display--letter-spacing);
		font-weight: var(--font-weight-semibold);
	}
	.cmp-h1:focus-visible {
		outline-width: var(--focus-ring-width);
		outline-style: solid;
		outline-color: var(--color-primary);
	}
	.cmp-manual {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: calc(var(--spacing) * 2);
		max-width: var(--prose-measure);
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-box);
		padding: calc(var(--spacing) * 3);
	}
	.cmp-field {
		width: 100%;
	}
	.cmp-stale {
		margin-bottom: calc(var(--spacing) * 4);
	}
	.cmp-skeleton {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 3);
	}
	.cmp-skeleton > :global(*) {
		height: calc(var(--spacing) * 12);
	}
</style>
