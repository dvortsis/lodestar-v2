<script lang="ts">
	// A string list as chips with Add (§10.10.4). Enter or Add appends; each chip removes itself.
	import Button from '$lib/ui/controls/Button.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Chip from '$lib/ui/labels/Chip.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { SettingsForm } from './form.svelte';
	import type { FieldKey } from './types';
	import type { FieldControlProps } from './widgets';

	interface Props {
		form: SettingsForm;
		key: FieldKey;
		controlProps: FieldControlProps;
		dense?: boolean;
	}

	let { form, key, controlProps, dense = false }: Props = $props();

	const f = $derived(form.field(key));
	const items = $derived(
		Array.isArray(f.value) ? f.value.filter((v): v is string => typeof v === 'string') : []
	);
	let draft = $state('');
	let input = $state<HTMLInputElement>();

	function add() {
		const v = draft.trim();
		if (!v || items.includes(v)) return;
		form.set(key, [...items, v]);
		draft = '';
	}
</script>

<div class="flex flex-col gap-2">
	{#if items.length}
		<ul class="flex flex-wrap gap-2" data-chip-group>
			{#each items as item, i (`${i}:${item}`)}
				<li>
					<Chip
						onremove={f.locked ? undefined : () => form.set(key, items.toSpliced(i, 1))}
						removeLabel={m.settings_form_chips_remove({ item })}
						fallbackFocus={() => input}>{item}</Chip
					>
				</li>
			{/each}
		</ul>
	{/if}
	{#if !f.locked}
		<div class="flex items-center gap-2">
			<Input
				{...controlProps}
				{dense}
				bind:value={draft}
				placeholder={m.settings_form_chips_new()}
				autocomplete="off"
				onkeydown={(e) => {
					if (e.key === 'Enter') {
						e.preventDefault();
						add();
					}
				}}
				onblur={() => form.blur(key)}
			/>
			<Button {dense} onclick={add}>{m.settings_form_chips_add()}</Button>
		</div>
	{/if}
</div>
