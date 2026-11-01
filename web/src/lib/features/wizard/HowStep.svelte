<script lang="ts">
	// "How it works" (§10.10.10): a static SVG sequence in the spirit of `infographic-design`, no
	// charting library, and the same four stages as an ordered list, which is its text equivalent.
	import { m } from '$lib/paraglide/messages.js';

	const stages = [
		{ title: () => m.wizard_how_dht(), text: () => m.wizard_how_dht_text() },
		{ title: () => m.wizard_how_metadata(), text: () => m.wizard_how_metadata_text() },
		{ title: () => m.wizard_how_index(), text: () => m.wizard_how_index_text() },
		{ title: () => m.wizard_how_search(), text: () => m.wizard_how_search_text() }
	];
	const X = [10, 130, 250, 370];
	const labels = [
		() => m.wizard_how_svg_dht(),
		() => m.wizard_how_svg_metadata(),
		() => m.wizard_how_svg_index(),
		() => m.wizard_how_svg_search()
	];
</script>

<svg
	viewBox="0 0 480 72"
	class="w-full max-w-lg text-base-content"
	aria-hidden="true"
	focusable="false"
>
	{#each X as x, i (x)}
		<rect
			{x}
			y="8"
			width="100"
			height="56"
			rx="8"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
		/>
		<text x={x + 50} y="26" text-anchor="middle" fill="currentColor" class="text-caption"
			>{i + 1}</text
		>
		<text
			x={x + 50}
			y="46"
			text-anchor="middle"
			fill="currentColor"
			class="text-small font-semibold">{labels[i]!()}</text
		>
		{#if i < 3}
			<path
				d="M{x + 104} 36 h12 m-5 -5 l5 5 l-5 5"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			/>
		{/if}
	{/each}
</svg>

<ol class="flex list-decimal flex-col gap-2 ps-4 text-body" data-testid="wizard-how">
	{#each stages as stage (stage.title())}
		<li><strong class="font-semibold">{stage.title()}.</strong> {stage.text()}</li>
	{/each}
</ol>

<p class="text-body" data-testid="wizard-how-cold">{m.wizard_how_cold()}</p>
