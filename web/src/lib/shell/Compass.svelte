<script lang="ts">
	// The brand compass (§10.19), drawn inline so its fills read the --brand-* tokens and follow the
	// theme. Same geometry as lib/brand/compass.svg (which carries the Riverstone fallbacks).
	let { class: className = 'size-8' }: { class?: string } = $props();
</script>

<svg viewBox="0 0 512 512" class="shrink-0 {className}" aria-hidden="true">
	<circle cx="256" cy="256" r="182" fill="var(--brand-surface)" />
	<path
		d="M99 257 A157 157 0 0 1 362 142 M413 258 A157 157 0 0 1 152 371"
		stroke="var(--brand-primary)"
		stroke-width="22"
		stroke-linecap="round"
		fill="none"
	/>
	<circle cx="115" cy="192" r="21" fill="var(--brand-accent)" />
	<circle cx="384" cy="340" r="23" fill="var(--brand-accent)" />
	<path
		d="M256 118 L290 218 L390 256 L290 290 L256 390 L221 290 L121 256 L221 218 Z"
		fill="var(--brand-star)"
	/>
	<circle cx="256" cy="256" r="28" fill="var(--brand-surface)" />
</svg>
