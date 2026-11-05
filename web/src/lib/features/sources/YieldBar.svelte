<script lang="ts">
	// The 24 h yield as a tiny in-house SVG (no chart library): the share of what a source found
	// that was stored, drawn as a bar under a track. `GET /sources` gives two 24 h counts and no
	// series, so this is a bar, not a line; the counts are written beside it by the row (the client
	// shows no percentage). Both counts null-safe: nothing found draws an empty track.
	interface Props {
		found: number;
		stored: number;
		label: string;
		testid?: string;
	}
	let { found, stored, label, testid = 'yield-bar' }: Props = $props();

	const W = 64;
	const part = $derived(
		found > 0 ? Math.min(W, Math.max(stored > 0 ? 1 : 0, (stored / found) * W)) : 0
	);
</script>

<svg
	class="h-2 w-(--compbar-min-width) shrink-0"
	viewBox="0 0 {W} 4"
	role="img"
	aria-label={label}
	data-testid={testid}
	data-stored={stored}
	data-found={found}
>
	<rect x="0" y="0" width={W} height="4" rx="2" fill="var(--data-1)" opacity="0.25" />
	{#if part > 0}<rect x="0" y="0" width={part} height="4" rx="2" fill="var(--data-1)" />{/if}
</svg>
