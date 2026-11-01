<script lang="ts">
	// §10.5.2 file tree. Starts from detail's first 500 files and pages the rest when this section
	// first scrolls into view. Torrents above 2,000 files build the tree (and re-run the filter) in
	// a module Web Worker, so the main thread only renders the visible rows.
	import { untrack } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Tree from '$lib/ui/lists/Tree.svelte';
	import { buildIndex, type FlatNode, type TreeIndex } from '$lib/ui/lists/tree-model';
	import type { TorrentDetail } from '../cache';
	import { buildNodes, type BuildOptions, type FileInput } from './build';
	import { WORKER_THRESHOLD, loadRest } from './load';
	import type { WorkerRequest, WorkerResult } from './tree.worker';

	interface Props {
		detail: Pick<TorrentDetail, 'hash' | 'file_list' | 'file_list_total'>;
		/** Positions of the files a search matched (`matched_files`). */
		matched?: readonly number[];
	}
	let { detail, matched = [] }: Props = $props();

	const locale = $derived(currentLocale());
	const hash = $derived(detail.hash);
	let workerFailed = $state(false);
	const useWorker = $derived(detail.file_list_total > WORKER_THRESHOLD && !workerFailed);

	// Every file held so far, kept here too so a failed worker can fall back to a main-thread build.
	let allFiles: FileInput[] = [];
	let building = false;
	let dirty = false;
	let loading = $state(false);
	let failed = $state(false);
	let showPadding = $state(false);
	let filterText = $state('');
	let filter = $state('');
	let nodes = $state.raw<FlatNode[]>([]);
	let index = $state.raw<TreeIndex | undefined>(undefined);
	let expanded = $state(new Set<string>());
	// eslint-disable-next-line svelte/prefer-svelte-reactivity -- plain bookkeeping, never rendered
	const autoOpened = new Set<string>();
	let worker: Worker | undefined;
	let seq = 0;
	let ctl: AbortController | undefined;

	const options = (): BuildOptions => ({
		showPadding,
		filter,
		matched,
		locale,
		labels: { executable: m.detail_tree_executable(), symlink: m.detail_tree_symlink() }
	});

	function apply(result: { nodes: FlatNode[]; expanded: string[]; index?: TreeIndex }) {
		nodes = result.nodes;
		index = result.index;
		// Open what is new; a folder the user closed stays closed.
		// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a copy handed back as the new value
		const open = new Set(expanded);
		let changed = false;
		for (const id of result.expanded) {
			if (!autoOpened.has(id)) {
				autoOpened.add(id);
				open.add(id);
				changed = true;
			}
		}
		if (changed) expanded = open;
	}

	function post(message: WorkerRequest) {
		// Plain data only: the file list and matches come from raw state, so they clone as they are.
		worker?.postMessage(message);
	}

	function startWorker() {
		const w = new Worker(new URL('./tree.worker.ts', import.meta.url), { type: 'module' });
		worker = w;
		w.onmessage = (event: MessageEvent<WorkerResult>) => {
			if (worker !== w) return;
			building = false;
			// Replies for an older query are dropped.
			if (event.data.seq === seq) apply(event.data);
			if (dirty) {
				dirty = false;
				rebuild();
			}
		};
		// A worker that can't start or can't answer must never read as "no files": build here instead.
		const fail = () => {
			if (worker !== w) return;
			w.terminate();
			worker = undefined;
			building = false;
			dirty = false;
			workerFailed = true;
			rebuild();
		};
		w.onerror = fail;
		w.onmessageerror = fail;
	}

	// One build in flight: files that arrive meanwhile are folded into the next build, so a 50,000
	// file torrent isn't rebuilt (and its tree cloned back) once per page.
	function rebuild() {
		if (worker) {
			if (building) {
				dirty = true;
				return;
			}
			building = true;
			post({ type: 'build', seq: ++seq, options: options() });
		} else {
			apply({ ...buildNodes(allFiles, options()), index: undefined });
		}
	}

	function addFiles(page: FileInput[]) {
		for (const f of page) allFiles.push(f);
		post({ type: 'append', files: page });
		loaded += page.length;
		rebuild();
	}

	let loaded = 0;

	async function page() {
		loading = true;
		failed = false;
		ctl?.abort();
		const mine = (ctl = new AbortController());
		try {
			await loadRest<FileInput>({
				loaded,
				total: detail.file_list_total,
				signal: mine.signal,
				fetchPage: async (offset, limit, signal) =>
					(
						await api.get('/torrents/{hash}/files', {
							path: { hash: detail.hash },
							query: { offset, limit },
							signal
						})
					).files,
				onPage: addFiles
			});
			if (!mine.signal.aborted) loading = false;
		} catch (e) {
			if (isAbort(e) || mine.signal.aborted) return;
			failed = true;
			loading = false;
		}
	}

	// (Re)start for a torrent: the first page is detail's own, then the rest is paged in.
	$effect(() => {
		void hash;
		untrack(() => {
			ctl?.abort();
			worker?.terminate();
			worker = undefined;
			autoOpened.clear();
			expanded = new Set();
			nodes = [];
			index = undefined;
			allFiles = [];
			loaded = 0;
			failed = false;
			loading = false;
			building = false;
			dirty = false;
			workerFailed = false;
			if (detail.file_list_total > WORKER_THRESHOLD) {
				startWorker();
				post({ type: 'reset' });
			}
			addFiles(detail.file_list as FileInput[]);
			if (loaded < detail.file_list_total) void page();
		});
		return () => {
			ctl?.abort();
			worker?.terminate();
			worker = undefined;
		};
	});

	// Filter: debounced 120 ms so typing never rebuilds per key; the padding toggle is immediate.
	let timer: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const text = filterText;
		clearTimeout(timer);
		timer = setTimeout(() => {
			filter = text;
		}, 120);
		return () => clearTimeout(timer);
	});
	let first = true;
	$effect(() => {
		void filter;
		void showPadding;
		void matched;
		if (first) {
			first = false;
			return;
		}
		untrack(() => {
			autoOpened.clear();
			rebuild();
		});
	});

	const treeIndex = $derived(index ?? (nodes.length ? buildIndex(nodes) : undefined));
	// Visible rows (a collapsed folder skips its subtree), capped at 10: the frame height.
	const rows = $derived.by(() => {
		let n = 0;
		for (let i = 0, x; treeIndex && (x = nodes[i]) && n < 10; n++)
			i = x.dir && !expanded.has(x.id) ? treeIndex.end[i]! : i + 1;
		return n;
	});
</script>

<div
	class="ls-detail-stack"
	data-testid="detail-file-tree"
	data-tree-worker={useWorker ? 'true' : 'false'}
	aria-busy={loading}
	style:--ls-tree-rows={rows}
>
	<div class="ls-detail-row">
		<div class="ls-detail-grow">
			<Input
				type="search"
				dense
				bind:value={filterText}
				aria-label={m.detail_tree_filter()}
				placeholder={m.detail_tree_filter_placeholder()}
				data-testid="detail-file-tree-filter"
			/>
		</div>
		<Checkbox label={m.detail_tree_show_padding()} bind:checked={showPadding} />
	</div>
	{#if loading}
		<p class="text-small text-muted-text" role="status" data-testid="detail-file-tree-loading">
			{m.detail_tree_loading({ total: formatCount(detail.file_list_total, locale) })}
		</p>
	{/if}
	{#if failed}
		<Banner kind="error">
			{m.detail_tree_error()}
			{#snippet action()}<Button dense onclick={page}>{m.detail_retry()}</Button>{/snippet}
		</Banner>
	{/if}
	{#if nodes.length}
		<Tree
			{nodes}
			index={treeIndex}
			label={m.detail_tree_label()}
			{expanded}
			onExpandedChange={(next) => (expanded = next)}
			markedLabel={m.detail_tree_matched()}
			class="ls-detail-tree"
		/>
	{:else if !loading}
		<p class="text-muted-text" data-testid="detail-file-tree-empty">{m.detail_tree_empty()}</p>
	{/if}
</div>
