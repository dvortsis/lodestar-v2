<script lang="ts">
	// A Menu opened from code (a button sets `open`), as the lazily loaded row menu is: no keydown
	// reaches Bits UI's keyboard tracker first. `autofocusFirst` and a disabled first item are options.
	import Menu from '../Menu.svelte';
	let {
		autofocusFirst = false,
		firstDisabled = false
	}: { autofocusFirst?: boolean; firstDisabled?: boolean } = $props();
	let open = $state(false);
</script>

<Menu
	label="Row actions"
	onSelect={() => {}}
	bind:open
	{autofocusFirst}
	items={[
		{ id: 'a', label: 'First', disabled: firstDisabled },
		{ id: 'b', label: 'Second' }
	]}
>
	{#snippet trigger({ props })}<button {...props} type="button">More</button>{/snippet}
</Menu>
<button type="button" onclick={() => (open = true)}>Open from code</button>
