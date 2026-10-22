<script lang="ts">
	import { DASH, formatCount, RANGE_SEPARATOR, type MetricValue } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import {
		clampRange,
		dragRange,
		moveEdge,
		nearestEdge,
		type BrushKey,
		type BrushRange
	} from './brush';
	import { lengthPct, numeric, type ValueFormatter } from './shared';

	interface Props {
		/** One count per bin, from the server. `null` is unknown; both it and 0 draw empty. */
		counts: MetricValue[];
		/** Labelled bin edges (`counts.length + 1`), formatted by the caller through `$lib/format`. */
		edges: string[];
		/** Accessible name of the chart. */
		label: string;
		/** Names for the two brush handles, e.g. "Lower bound" / "Upper bound". Omit to disable brushing. */
		handleLabels?: { from: string; to: string };
		/** Selected bin range as edge indexes (`from < to`); bindable. Clamped when the bins change. */
		selection?: BrushRange | null;
		/** Every change of the range (drag, keys): cheap, for local feedback. */
		onbrush?: (range: BrushRange | null) => void;
		/** The range is settled (pointer released, key pressed, cleared): the moment to query. */
		oncommit?: (range: BrushRange | null) => void;
		format?: ValueFormatter;
		testid?: string;
	}
	let {
		counts,
		edges,
		label,
		handleLabels,
		selection = $bindable(null),
		onbrush,
		oncommit,
		format = formatCount,
		testid = 'histogram'
	}: Props = $props();

	const locale = $derived(currentLocale());
	const bins = $derived(counts.length);
	const peak = $derived(Math.max(0, ...counts.map((c) => numeric(c) ?? 0)));
	// A range stored for a different bin count is clamped (or dropped) rather than drawn outside.
	const sel = $derived(selection ? clampRange(selection, bins) : null);

	// Label as many edges as fit without touching: the stride follows the longest label and the
	// measured width; the last edge always shows and displaces a neighbour that would overlap it.
	let rowWidth = $state(0);
	let row = $state<HTMLElement>();
	$effect(() => {
		if (!row || typeof ResizeObserver === 'undefined') return;
		const node = row;
		const watch = new ResizeObserver(() => (rowWidth = node.clientWidth));
		watch.observe(node);
		rowWidth = node.clientWidth;
		return () => watch.disconnect();
	});
	const CHAR = 7; // px per caption character, an upper bound for Atkinson at 12px
	const stride = $derived.by(() => {
		const longest = Math.max(1, ...edges.map((e) => e.length));
		const per = longest * CHAR + 8;
		const fit = rowWidth > 0 ? Math.max(1, Math.floor(rowWidth / per)) : 2;
		return Math.max(1, Math.ceil((edges.length - 1) / Math.max(1, fit - 1)));
	});
	const shown = $derived(
		edges
			.map((text, i) => ({ text, i }))
			.filter(
				({ i }) =>
					i === edges.length - 1 ||
					(i % stride === 0 && (edges.length - 1 - i >= stride || i === 0 || stride === 1))
			)
	);

	let plot = $state<HTMLElement>();
	// What the current pointer drag moves: a fresh selection from an anchor, or one handle.
	let drag: { kind: 'new'; anchor: number } | { kind: 'from' | 'to' } | null = null;

	const same = (a: BrushRange | null, b: BrushRange | null) =>
		a === b || (!!a && !!b && a.from === b.from && a.to === b.to);

	function set(range: BrushRange | null, commit: boolean) {
		const changed = !same(sel, range);
		if (changed) {
			selection = range;
			onbrush?.(range);
		}
		if (commit) oncommit?.(range);
	}

	function edgeAt(event: PointerEvent): number {
		const box = plot!.getBoundingClientRect();
		return nearestEdge(box.width > 0 ? (event.clientX - box.left) / box.width : 0, bins);
	}
	function down(event: PointerEvent) {
		if (!handleLabels || !plot || bins < 1) return;
		const anchor = edgeAt(event);
		drag = { kind: 'new', anchor };
		plot.setPointerCapture?.(event.pointerId);
		set(dragRange(anchor, anchor, bins), false);
	}
	// A handle moves only its own edge; the press must not reach the plot, which would restart.
	function handleDown(event: PointerEvent, edge: 'from' | 'to') {
		if (!plot || !sel) return;
		event.stopPropagation();
		drag = { kind: edge };
		plot.setPointerCapture?.(event.pointerId);
	}
	function move(event: PointerEvent) {
		if (!drag || !plot) return;
		const at = edgeAt(event);
		if (drag.kind === 'new') set(dragRange(drag.anchor, at, bins), false);
		else if (sel)
			set(
				drag.kind === 'from'
					? { from: Math.min(at, sel.to - 1), to: sel.to }
					: { from: sel.from, to: Math.max(at, sel.from + 1) },
				false
			);
	}
	function up() {
		if (!drag) return;
		drag = null;
		oncommit?.(sel);
	}

	function plotKey(event: KeyboardEvent) {
		// Keyboard entry into the brush: Enter or an arrow selects every bin; the handles take over.
		if (event.target !== plot || sel || !handleLabels || bins < 1) return;
		if (!['Enter', 'ArrowLeft', 'ArrowRight'].includes(event.key)) return;
		set({ from: 0, to: bins }, true);
		event.preventDefault();
	}
	function key(event: KeyboardEvent, edge: 'from' | 'to') {
		if (!sel) return;
		if (event.key === 'Escape') {
			set(null, true);
			plot?.focus();
		} else if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
			set(moveEdge(sel, edge, event.key as BrushKey, bins), true);
		} else return;
		event.preventDefault();
	}
	const pos = (edge: number) => `${bins ? (edge / bins) * 100 : 0}%`;
	const FOCUS =
		'focus-visible:outline-(length:--focus-ring-width) focus-visible:outline-solid focus-visible:outline-primary focus-visible:outline-offset-(--focus-ring-offset)';
</script>

<div class="flex flex-col gap-1" data-testid={testid}>
	<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
	<div
		bind:this={plot}
		class="relative flex h-14 items-end border-b border-field-border {FOCUS} {handleLabels
			? 'touch-none cursor-crosshair'
			: ''}"
		role="group"
		aria-label={label}
		tabindex={handleLabels ? 0 : undefined}
		onkeydown={plotKey}
		onpointerdown={down}
		onpointermove={move}
		onpointerup={up}
		onpointercancel={up}
	>
		{#each counts as count, i (i)}
			{@const n = numeric(count)}
			<div
				class="flex h-full min-w-0 flex-1 items-end px-px"
				data-testid="{testid}-bin"
				data-empty={n === null || n === 0 ? 'true' : 'false'}
				title="{edges[i] ?? DASH}{RANGE_SEPARATOR}{edges[i + 1] ?? DASH}: {format(count, locale)}"
			>
				{#if n !== null && n > 0}
					<div
						class="min-h-px w-full rounded-t-selector"
						style:height={lengthPct(n, peak)}
						style:background-color="var(--data-1)"
					></div>
				{/if}
			</div>
		{/each}
		{#if sel && handleLabels}
			<div
				class="pointer-events-none absolute inset-y-0 bg-selected opacity-60"
				style:left={pos(sel.from)}
				style:width={pos(sel.to - sel.from)}
				data-testid="{testid}-selection"
			></div>
			{#each ['from', 'to'] as const as edge (edge)}
				<div
					class="absolute inset-y-0 -ml-1 w-2 cursor-ew-resize rounded-selector bg-primary {FOCUS}"
					style:left={pos(sel[edge])}
					role="slider"
					tabindex="0"
					aria-label={handleLabels[edge]}
					aria-orientation="horizontal"
					aria-valuemin={edge === 'from' ? 0 : sel.from + 1}
					aria-valuemax={edge === 'from' ? sel.to - 1 : bins}
					aria-valuenow={sel[edge]}
					aria-valuetext={edges[sel[edge]]}
					data-testid="{testid}-handle-{edge}"
					onkeydown={(e) => key(e, edge)}
					onpointerdown={(e) => handleDown(e, edge)}
				></div>
			{/each}
		{/if}
	</div>
	<div
		class="relative h-4 text-caption text-muted-text tabular-nums"
		aria-hidden="true"
		bind:this={row}
	>
		{#each shown as { text, i } (i)}
			<span
				class="absolute whitespace-nowrap {i === 0
					? ''
					: i === edges.length - 1
						? '-translate-x-full'
						: '-translate-x-1/2'}"
				style:left={pos(i)}
				data-testid="{testid}-edge">{text}</span
			>
		{/each}
	</div>
</div>
