<script lang="ts">
	// Test-only showcase of the static tokens (docs/design-tokens.md §6, §8). Not linked anywhere.
	const scale = [
		{ name: 'caption', cls: 'text-caption', spec: '12 / 16' },
		{ name: 'small', cls: 'text-small', spec: '13 / 18' },
		{ name: 'body', cls: 'text-body', spec: '14 / 20' },
		{ name: 'body-lg', cls: 'text-body-lg', spec: '16 / 24' },
		{ name: 'title', cls: 'text-title', spec: '20 / 26' },
		{ name: 'display', cls: 'text-display', spec: '28 / 32 / -0.02em' }
	];
	const weights = [
		{ name: 'normal', cls: 'font-normal' },
		{ name: 'medium', cls: 'font-medium' },
		{ name: 'semibold', cls: 'font-semibold' }
	];
	const radii = [
		{ name: 'sheet', cls: 'rounded-sheet', px: 16 },
		{ name: 'box', cls: 'rounded-box', px: 12 },
		{ name: 'field', cls: 'rounded-field', px: 8 },
		{ name: 'selector', cls: 'rounded-selector', px: 4 }
	];
</script>

<div
	class="mx-auto flex max-w-(--manuscript-width) flex-col gap-8 bg-base-200 p-6 text-base-content"
>
	<section class="flex flex-col gap-3" data-testid="type-scale">
		<h1 class="text-display font-semibold">Token showcase</h1>
		{#each scale as step (step.name)}
			<div class="flex items-baseline gap-4" data-testid="type-{step.name}">
				<span class="w-(--rail-width) shrink-0 font-mono text-small text-muted-text"
					>{step.name} {step.spec}</span
				>
				<span class={step.cls}>Every node is a new discovery</span>
			</div>
		{/each}
		{#each weights as w (w.name)}
			<div class="text-body {w.cls}" data-testid="weight-{w.name}">{w.name}: 1,234,567 files</div>
		{/each}
		<div class="text-body tabular-nums" data-testid="tabular">
			<div>1,111,111</div>
			<div>0,000,000</div>
		</div>
		<code class="font-mono text-small" data-testid="mono"
			>3b245504cf5f11bbdbe1201cea6a6bf45aee1bc0</code
		>
		<p class="text-body" data-testid="latin-ext">Łódź · Ångström · ăâîșț · ñ</p>
	</section>

	<section class="flex flex-wrap gap-4" data-testid="radii">
		{#each radii as r (r.name)}
			<div
				class="flex h-14 w-(--compbar-max-width) items-center justify-center border border-base-300 bg-base-100 text-small {r.cls}"
				data-testid="radius-{r.name}"
			>
				{r.name}
				{r.px}
			</div>
		{/each}
	</section>

	<section class="flex flex-col gap-3" data-testid="controls">
		<div class="flex flex-wrap items-center gap-3">
			<button class="btn btn-primary" data-testid="control-md">Default 40</button>
			<button class="btn btn-sm" data-testid="control-sm">Dense 32</button>
			<input class="input" placeholder="input md" data-testid="input-md" />
			<input class="input input-sm" placeholder="input sm" data-testid="input-sm" />
		</div>
		<div
			class="flex items-center rounded-field border border-field-border bg-base-100 px-3 text-body-lg"
			style:height="var(--query-bar-height)"
			data-testid="query-bar"
		>
			Query bar 48
		</div>
		<div class="flex flex-wrap items-center gap-3">
			<span class="badge badge-neutral">neutral</span>
			<span class="badge badge-success">success</span>
			<span class="badge badge-warning">warning</span>
			<span class="badge badge-error">error</span>
		</div>
	</section>
</div>
