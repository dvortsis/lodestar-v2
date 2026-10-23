<script lang="ts">
	// §10.5.6 raw viewer: any decoded JSON value as a collapsible mono tree. Text only: every value
	// is rendered through Svelte's escaping, so a string like "<script>" is shown, never run. Long
	// strings are cut with "Show all", long lists with a count, and depth is limited. Not mounted in
	// the drawer yet (no endpoint serves an info dict); the dev harness and tests use it.
	import IconCaretRight from '~icons/ph/caret-right';
	import { m } from '$lib/paraglide/messages.js';

	interface Props {
		value: unknown;
		/** Accessible name of the tree. */
		label?: string;
		maxDepth?: number;
		maxString?: number;
		maxItems?: number;
	}
	let { value, label, maxDepth = 8, maxString = 200, maxItems = 100 }: Props = $props();

	/* Plain sets replaced on every change (never mutated), so the template re-renders. */
	/* eslint-disable svelte/prefer-svelte-reactivity */
	let open = $state<Set<string>>(new Set(['$']));
	let full = $state<Set<string>>(new Set());
	let more = $state<Set<string>>(new Set());

	const flip = (set: Set<string>, key: string) => {
		const next = new Set(set);
		if (!next.delete(key)) next.add(key);
		return next;
	};
	/* eslint-enable svelte/prefer-svelte-reactivity */
	const isBranch = (v: unknown): v is object => typeof v === 'object' && v !== null;
	const entries = (v: object): [string, unknown][] =>
		Array.isArray(v) ? v.map((x, i) => [String(i), x]) : Object.entries(v);
	const scalar = (v: unknown): string =>
		typeof v === 'string' ? JSON.stringify(v) : v === undefined ? 'undefined' : String(v);
</script>

{#snippet node(v: unknown, name: string, path: string, depth: number)}
	<li>
		{#if isBranch(v)}
			{@const list = entries(v)}
			{@const isOpen = open.has(path)}
			<button
				type="button"
				class="ls-focus inline-flex items-center gap-1"
				aria-expanded={isOpen}
				onclick={() => (open = flip(open, path))}
			>
				<IconCaretRight
					class="size-(--icon-size-row) {isOpen ? 'rotate-90' : ''}"
					aria-hidden="true"
				/>
				<span>{name}</span>
				<span class="text-muted-text"
					>{Array.isArray(v) ? `[${list.length}]` : `{${list.length}}`}</span
				>
			</button>
			{#if isOpen}
				{#if depth >= maxDepth}
					<span class="text-muted-text"> {m.detail_raw_depth()}</span>
				{:else if list.length === 0}
					<span class="text-muted-text"> {m.detail_raw_empty()}</span>
				{:else}
					<ul>
						{#each more.has(path) ? list : list.slice(0, maxItems) as [k, child] (k)}
							{@render node(child, k, `${path}.${k}`, depth + 1)}
						{/each}
					</ul>
					{#if !more.has(path) && list.length > maxItems}
						<button
							type="button"
							class="ls-focus ls-detail-link"
							onclick={() => (more = flip(more, path))}
						>
							{m.detail_raw_show_all()} ({list.length})
						</button>
					{/if}
				{/if}
			{/if}
		{:else if typeof v === 'string' && v.length > maxString && !full.has(path)}
			<span>{name}: {JSON.stringify(v.slice(0, maxString))}…</span>
			<span class="text-muted-text"> ({m.detail_raw_truncated({ count: String(v.length) })})</span>
			<button
				type="button"
				class="ls-focus ls-detail-link"
				onclick={() => (full = flip(full, path))}
			>
				{m.detail_raw_show_all()}
			</button>
		{:else}
			<span>{name}: {scalar(v)}</span>
		{/if}
	</li>
{/snippet}

<ul class="ls-detail-raw" aria-label={label ?? m.detail_raw_label()} data-testid="detail-raw">
	{@render node(value, label ?? m.detail_raw_label(), '$', 0)}
</ul>
