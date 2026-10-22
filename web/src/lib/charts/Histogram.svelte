<script lang="ts">
	import { formatCount, type MetricValue } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { dragRange, moveEdge, nearestEdge, type BrushKey, type BrushRange } from './brush';
	import { lengthPct, numeric, type ValueFormatter } from './shared';

	interface Props {
		/** One count per bin, from the server. `null` is unknown; both it and 0 draw empty. */
		counts: MetricValue[];
		/** Labelled bin edges (`counts.length + 1`), formatted by the caller through `$lib/format`. */
		edges: string[];
		/** Accessible name of the chart. */
		label: string;
		/** Names for the two brush handles, e.g. "Lower bound" / "Upper bound". */
		handleLabels?: { from: string; to: string };
		/** Selected bin range as edge indexes; bindable. Omit `handleLabels` to disable brushing. */
		selection?: BrushRange | null;
		onbrush?: (range: BrushRange | null) => void;
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
		format = formatCount,
		testid = 'histogram'
	}: Props = $props();

	const locale = $derived(currentLocale());
	const bins = $derived(counts.length);
	const peak = $derived(Math.max(0, ...counts.map((c) => numeric(c) ?? 0)));
	// Label every edge when there is room, otherwise thin them out; first and last always show.
	const stride = $derived(Math.max(1, Math.ceil(edges.length / 6)));
	const shown = $derived(
		edges
			.map((text, i) => ({ text, i }))
			.filter(({ i }) => i % stride === 0 || i === edges.length - 1)
	);

	let plot = $state<HTMLElement>();
	let anchor: number | null = null;

	function commit(range: BrushRange | null) {
		selection = range;
		onbrush?.(range);
	}

	function edgeAt(event: PointerEvent): number {
		const box = plot!.getBoundingClientRect();
		return nearestEdge(box.width > 0 ? (event.clientX - box.left) / box.width : 0, bins);
	}
	function down(event: PointerEvent) {
		if (!handleLabels || !plot || bins < 1) return;
		anchor = edgeAt(event);
		plot.setPointerCapture?.(event.pointerId);
		commit(dragRange(anchor, anchor, bins));
	}
	function move(event: PointerEvent) {
		if (anchor === null) return;
		commit(dragRange(anchor, edgeAt(event), bins));
	}
	function up() {
		anchor = null;
	}

	function plotKey(event: KeyboardEvent) {
		// Keyboard entry into the brush: Enter or an arrow selects every bin; the handles take over.
		if (event.target !== plot || selection || !handleLabels || bins < 1) return;
		if (!['Enter', 'ArrowLeft', 'ArrowRight'].includes(event.key)) return;
		commit({ from: 0, to: bins });
		event.preventDefault();
	}
	function key(event: KeyboardEvent, edge: 'from' | 'to') {
		if (!selection) return;
		if (event.key === 'Escape') {
			commit(null);
			plot?.focus();
		} else if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
			commit(moveEdge(selection, edge, event.key as BrushKey, bins));
		} else return;
		event.preventDefault();
	}
	const pos = (edge: number) => `${bins ? (edge / bins) * 100 : 0}%`;
</script>

<div class="flex flex-col gap-1" data-testid={testid}>
	<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
	<div
		bind:this={plot}
		class="relative flex h-14 items-end border-b border-field-border {handleLabels
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
				title="{edges[i]} – {edges[i + 1]}: {format(count, locale)}"
			>
				{#if n !== null && n > 0}
					<div
						class="w-full min-h-px rounded-t-selector"
						style:height={lengthPct(n, peak)}
						style:background-color="var(--data-1)"
					></div>
				{/if}
			</div>
		{/each}
		{#if selection && handleLabels}
			<div
				class="pointer-events-none absolute inset-y-0 bg-selected opacity-60"
				style:left={pos(selection.from)}
				style:width={pos(selection.to - selection.from)}
				data-testid="{testid}-selection"
			></div>
			{#each ['from', 'to'] as const as edge (edge)}
				<div
					class="absolute inset-y-0 -ml-1 w-2 cursor-ew-resize rounded-selector bg-primary focus-visible:outline-(length:--focus-ring-width) focus-visible:outline-offset-(--focus-ring-offset)"
					style:left={pos(selection[edge])}
					role="slider"
					tabindex="0"
					aria-label={handleLabels[edge]}
					aria-orientation="horizontal"
					aria-valuemin={edge === 'from' ? 0 : selection.from + 1}
					aria-valuemax={edge === 'from' ? selection.to - 1 : bins}
					aria-valuenow={selection[edge]}
					aria-valuetext={edges[selection[edge]]}
					data-testid="{testid}-handle-{edge}"
					onkeydown={(e) => key(e, edge)}
				></div>
			{/each}
		{/if}
	</div>
	<div class="relative h-4 text-caption text-muted-text tabular-nums" aria-hidden="true">
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
