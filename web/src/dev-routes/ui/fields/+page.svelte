<script lang="ts">
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Textarea from '$lib/ui/fields/Textarea.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Combobox from '$lib/ui/fields/Combobox.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Radio from '$lib/ui/fields/Radio.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import Slider from '$lib/ui/fields/Slider.svelte';
	import RangeInput from '$lib/ui/fields/RangeInput.svelte';

	let name = $state('');
	let tracker = $state('');
	let radio = $state('a');
	let paused = $state(false);
	let rate = $state(40);
	let min = $state<number | null>(100);
	let max = $state<number | null>(50);
	const trackers = ['opentrackr', 'torrent.eu.org', 'tracker.moeking.me'].map((t) => ({
		value: t,
		label: t
	}));
</script>

<div
	class="mx-auto flex max-w-(--manuscript-width) flex-col gap-5 bg-base-200 p-6 text-base-content"
>
	<h1 class="text-display font-semibold">Fields</h1>

	<Field label="Name" help="Shown in lists" required error={name ? undefined : 'Enter a name'}>
		{#snippet children(props)}<Input {...props} bind:value={name} />{/snippet}
	</Field>
	<Field label="Dense input" help="32 px">
		{#snippet children(props)}<Input {...props} dense mono value="3b245504cf5f" />{/snippet}
	</Field>
	<Field label="Notes">
		{#snippet children(props)}<Textarea {...props} rows={3} />{/snippet}
	</Field>
	<Field label="Sort by">
		{#snippet children(props)}
			<Select
				{...props}
				placeholder="Choose…"
				options={[
					{ value: 'seeders', label: 'Seeders' },
					{ value: 'size', label: 'Size' }
				]}
			/>
		{/snippet}
	</Field>
	<Field label="Tracker" help="Type to filter, arrow keys to move">
		{#snippet children(props)}
			<Combobox
				{...props}
				label="Tracker"
				options={trackers}
				bind:value={tracker}
				placeholder="Tracker"
			/>
		{/snippet}
	</Field>

	<fieldset class="flex flex-col gap-1">
		<legend class="text-small font-medium">Mode</legend>
		<Radio name="mode" label="Automatic" value="a" bind:group={radio} />
		<Radio name="mode" label="Manual" value="b" bind:group={radio} />
	</fieldset>
	<Checkbox label="Include adult content" checked={false} />
	<Checkbox label="Some selected" indeterminate />
	<Toggle label="Pause crawler" bind:checked={paused} />
	<Toggle label="Disabled toggle" disabled />

	<Field label="Bandwidth limit">
		{#snippet children(props)}
			<Slider {...props} bind:value={rate} min={0} max={200} unit="MiB/s" />
		{/snippet}
	</Field>
	<RangeInput
		label="Size"
		minLabel="Minimum size"
		maxLabel="Maximum size"
		unit="MiB"
		bind:min
		bind:max
		invalidMessage="Minimum is larger than maximum"
	/>
</div>
