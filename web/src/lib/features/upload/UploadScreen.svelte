<script lang="ts">
	// Settings → Add torrent files (UP-2, UI review 02 §5.3): a drop zone with a visible Choose
	// files button (drag is never the only way), the limits named before anything is sent, one
	// result per file with Retry where the server or the connection failed, and the watch folder's
	// settings below. Files go one at a time, in the order picked, so a pick never races itself
	// past the server's rate limit; leaving the screen stops what hasn't been sent.
	import { onDestroy, onMount, type Component } from 'svelte';
	import { resolve } from '$app/paths';
	import { formatBytes, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge, { type BadgeVariant } from '$lib/ui/labels/Badge.svelte';
	import { PAGE_H1 } from '$lib/ui/layout/classes';
	import { FOCUS_RING } from '$lib/ui/classes';
	import {
		MAX_BYTES,
		MAX_FILES,
		RETRYABLE,
		precheck,
		send,
		tally,
		type Item,
		type Outcome,
		type State
	} from './upload';

	const locale = () => currentLocale();
	const count = (n: number) => formatCount(n, locale());
	const limit = $derived(formatBytes(MAX_BYTES, locale()));

	let items = $state<Item[]>([]);
	let tooMany = $state(0);
	let dragging = $state(false);
	let announcement = $state('');
	let input = $state<HTMLInputElement>();
	// The watch folder's part loads after the upload part paints. Not through Settings' Lazy: its
	// failure line would put that message in a chunk of its own that every Settings page fetches.
	let WatchFolder = $state<Component>();
	let watchFailed = $state(false);
	onMount(() => {
		void import('./WatchFolder.svelte').then(
			(mod) => (WatchFolder = mod.default),
			() => (watchFailed = true)
		);
	});
	let nextId = 0;
	let running = false;
	let ctl: AbortController | undefined = new AbortController();
	onDestroy(() => {
		ctl?.abort();
		ctl = undefined;
	});

	const ORDER: State[] = [
		'added',
		'malformed',
		'held',
		'refused',
		'too_large',
		'rate_limited',
		'unavailable',
		'failed',
		'stopped',
		'sending',
		'queued'
	];
	const variant: Record<State, BadgeVariant> = {
		queued: 'neutral',
		sending: 'info',
		added: 'success',
		malformed: 'error',
		held: 'warning',
		refused: 'error',
		too_large: 'error',
		rate_limited: 'warning',
		unavailable: 'warning',
		failed: 'error',
		stopped: 'neutral'
	};
	const stateText = (i: Item): string => {
		switch (i.state) {
			case 'added':
				return i.private ? m.upload_state_added_private() : m.upload_state_added();
			case 'too_large':
				return m.upload_state_too_large({ size: limit });
			case 'rate_limited':
				return i.wait
					? m.upload_state_rate_limited_wait({ seconds: count(i.wait) })
					: m.upload_state_rate_limited();
			default:
				return {
					queued: m.upload_state_queued,
					sending: m.upload_state_sending,
					malformed: m.upload_state_malformed,
					held: m.upload_state_held,
					refused: m.upload_state_refused,
					unavailable: m.upload_state_unavailable,
					failed: m.upload_state_failed,
					stopped: m.upload_state_stopped
				}[i.state]();
		}
	};
	const countText: Record<State, (n: { n: string }) => string> = {
		queued: m.upload_count_queued,
		sending: m.upload_count_sending,
		added: m.upload_count_added,
		malformed: m.upload_count_malformed,
		held: m.upload_count_held,
		refused: m.upload_count_refused,
		too_large: m.upload_count_too_large,
		rate_limited: m.upload_count_retry,
		unavailable: m.upload_count_retry,
		failed: m.upload_count_retry,
		stopped: m.upload_count_retry
	};

	/** Counts in the header; the four retryable states share one line. */
	const summary = $derived.by(() => {
		const t = tally(items);
		const retry = [...RETRYABLE].reduce((n, s) => n + (t[s] ?? 0), 0);
		const out: { state: State; n: number }[] = [];
		for (const s of ORDER) {
			const n = s === 'failed' ? retry : RETRYABLE.has(s) ? 0 : (t[s] ?? 0);
			if (n) out.push({ state: s, n });
		}
		return out;
	});
	const anyRetry = $derived(items.some((i) => RETRYABLE.has(i.state)));
	const busy = $derived(items.some((i) => i.state === 'queued' || i.state === 'sending'));

	function apply(item: Item, o: Outcome) {
		item.state = o.state;
		item.hash = o.state === 'added' ? o.hash : undefined;
		item.private = o.state === 'added' ? o.private : undefined;
		item.wait = o.state === 'rate_limited' ? o.wait : undefined;
	}

	/** Adds a pick: refused whole, with the limit named, when it holds more than `MAX_FILES`. */
	function add(list: FileList | File[] | null | undefined) {
		const files = [...(list ?? [])];
		if (!files.length) return;
		if (files.length > MAX_FILES) {
			tooMany = files.length;
			return;
		}
		tooMany = 0;
		for (const file of files) {
			const item: Item = { id: nextId++, file, state: 'queued' };
			const early = precheck(file);
			if (early) apply(item, early);
			items.push(item);
		}
		void pump();
	}

	/** Sends queued files one at a time. A rate limit or a server that's not taking uploads stops
	 * the rest, which wait for Retry instead of failing the same way one by one. */
	async function pump() {
		if (running) return;
		running = true;
		try {
			for (;;) {
				const item = items.find((i) => i.state === 'queued');
				if (!item || !ctl) break;
				item.state = 'sending';
				let o: Outcome;
				try {
					o = await send(item.file, ctl.signal);
				} catch {
					return; // the screen closed
				}
				apply(item, o);
				if (o.state === 'rate_limited' || o.state === 'unavailable')
					for (const rest of items) if (rest.state === 'queued') rest.state = 'stopped';
			}
		} finally {
			running = false;
		}
		if (ctl) announcement = summary.map((s) => countText[s.state]({ n: count(s.n) })).join(', ');
	}

	function retry(item?: Item) {
		for (const i of item ? [item] : items) if (RETRYABLE.has(i.state)) i.state = 'queued';
		void pump();
	}

	function clear() {
		items = items.filter((i) => i.state === 'queued' || i.state === 'sending');
		tooMany = 0;
		announcement = '';
	}

	const hasFiles = (e: DragEvent) => [...(e.dataTransfer?.types ?? [])].includes('Files');
	// The whole screen takes a drop, so a file let go beside the zone is added, not opened by the
	// browser in place of the app.
	function over(e: DragEvent) {
		if (!hasFiles(e)) return;
		e.preventDefault();
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
		dragging = true;
	}
	function drop(e: DragEvent) {
		if (!hasFiles(e)) return;
		e.preventDefault();
		dragging = false;
		add(e.dataTransfer?.files);
	}
	function leave(e: DragEvent) {
		// Leaving for a child of the page isn't leaving.
		if (e.relatedTarget === null) dragging = false;
	}
</script>

<svelte:window ondragover={over} ondrop={drop} ondragleave={leave} />

<h1 class={PAGE_H1} tabindex="-1">{m.upload_title()}</h1>
<p class="text-muted-text">{m.upload_purpose()}</p>

<section class="flex flex-col gap-4" aria-labelledby="upload-files">
	<h2 id="upload-files" class="text-body-lg font-semibold">{m.upload_files_heading()}</h2>
	<div
		class="flex flex-col items-start gap-3 rounded-box border p-6 {dragging
			? 'border-primary bg-selected'
			: 'border-field-border'}"
		data-testid="upload-zone"
		data-dragging={dragging || undefined}
	>
		<p>{m.upload_drop_hint()}</p>
		<input
			bind:this={input}
			type="file"
			multiple
			accept=".torrent,application/x-bittorrent"
			class="sr-only"
			tabindex="-1"
			aria-hidden="true"
			data-testid="upload-input"
			onchange={(e) => {
				add(e.currentTarget.files);
				e.currentTarget.value = '';
			}}
		/>
		<Button variant="primary" onclick={() => input?.click()} data-testid="upload-choose"
			>{m.upload_choose()}</Button
		>
		<p class="text-small text-muted-text" data-testid="upload-limits">
			{m.upload_limits({ max: count(MAX_FILES), size: limit })}
		</p>
	</div>

	{#if tooMany}
		<p role="alert" class="text-small text-error" data-testid="upload-too-many">
			{m.upload_too_many({ n: count(tooMany), max: count(MAX_FILES) })}
		</p>
	{/if}

	<p class="sr-only" role="status" data-testid="upload-announce">{announcement}</p>

	{#if items.length}
		<div class="flex flex-wrap items-center justify-between gap-2">
			<h3 class="text-body font-semibold">{m.upload_results_heading()}</h3>
			<div class="flex flex-wrap gap-2">
				{#if anyRetry && !busy}<Button dense onclick={() => retry()} data-testid="upload-retry-all"
						>{m.upload_retry_all()}</Button
					>{/if}
				{#if !busy}<Button dense variant="quiet" onclick={clear} data-testid="upload-clear"
						>{m.upload_clear()}</Button
					>{/if}
			</div>
		</div>
		<ul class="flex flex-wrap gap-x-4 gap-y-1 text-small" data-testid="upload-summary">
			{#each summary as s (s.state)}
				<li data-testid="upload-count-{s.state}">{countText[s.state]({ n: count(s.n) })}</li>
			{/each}
		</ul>
		<ul class="flex flex-col" aria-label={m.upload_results_heading()} data-testid="upload-results">
			{#each items as item (item.id)}
				<li
					class="flex flex-wrap items-center justify-between gap-2 border-b border-base-300 py-2"
					data-testid="upload-item"
					data-state={item.state}
				>
					<div class="flex min-w-0 flex-col">
						<!-- A file name can hold direction marks: isolated, so it can't reorder the row. -->
						<bdi class="wrap-anywhere">{item.file.name}</bdi>
						<span class="text-small text-muted-text" data-testid="upload-item-size"
							>{formatBytes(item.file.size, locale())}</span
						>
					</div>
					<div class="flex flex-wrap items-center gap-2">
						<Badge variant={variant[item.state]}
							><span data-testid="upload-item-state">{stateText(item)}</span></Badge
						>
						{#if item.state === 'added' && item.hash}
							<a
								class="link text-small {FOCUS_RING}"
								href={resolve('/t/[hash]', { hash: item.hash })}
								aria-label={m.upload_open_label({ name: item.file.name })}
								data-testid="upload-open">{m.upload_open()}</a
							>
						{:else if RETRYABLE.has(item.state) && !busy}
							<Button
								dense
								onclick={() => retry(item)}
								aria-label={m.upload_retry_label({ name: item.file.name })}
								data-testid="upload-retry">{m.upload_retry()}</Button
							>
						{/if}
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</section>

{#if WatchFolder}
	<WatchFolder />
{:else if watchFailed}
	<p role="alert" class="text-small text-error" data-testid="watch-load-failed">
		{m.upload_watch_load_failed()}
	</p>
{/if}
