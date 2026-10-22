<script lang="ts">
	import Button from '$lib/ui/controls/Button.svelte';
	import IconButton from '$lib/ui/controls/IconButton.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import IconGear from '~icons/ph/gear';

	const variants = ['primary', 'secondary', 'quiet', 'danger'] as const;
	let density = $state('comfortable');
	let loading = $state(false);
</script>

<div
	class="mx-auto flex max-w-(--manuscript-width) flex-col gap-6 bg-base-200 p-6 text-base-content"
>
	<h1 class="text-display font-semibold">Controls</h1>

	{#each [false, true] as dense (dense)}
		<section
			class="flex flex-wrap items-center gap-3"
			data-testid="buttons-{dense ? 'dense' : 'default'}"
		>
			{#each variants as variant (variant)}
				<Button {variant} {dense}>{variant}</Button>
			{/each}
			<Button variant="primary" {dense} disabled>Disabled</Button>
		</section>
	{/each}

	<section class="flex flex-wrap items-center gap-3">
		<Button
			variant="primary"
			{loading}
			onclick={() => (loading = true)}
			data-testid="loading-button"
		>
			Save changes
		</Button>
		<Button variant="quiet" onclick={() => (loading = false)}>Reset</Button>
		<IconButton label="Settings"><IconGear class="size-5" /></IconButton>
		<IconButton label="Settings (dense, secondary)" variant="secondary" dense>
			<IconGear class="size-4" />
		</IconButton>
	</section>

	<section class="flex flex-col gap-2">
		<SegmentedControl
			label="Density"
			bind:value={density}
			options={[
				{ value: 'comfortable', label: 'Comfortable' },
				{ value: 'compact', label: 'Compact' },
				{ value: 'cards', label: 'Cards' },
				{ value: 'table', label: 'Table', disabled: true }
			]}
		/>
		<p class="text-small text-muted-text" data-testid="density-value">Selected: {density}</p>
	</section>
</div>
