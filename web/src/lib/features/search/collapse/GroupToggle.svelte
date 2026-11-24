<script lang="ts">
	// The toolbar's "Group versions" switch (§10.4: a parameter, not a filter). Its label says what
	// on means (UI review I4). While group data isn't ready the switch stays usable and the toolbar
	// says why the rows come ungrouped (owner §2.66 item 2; `describedby` names that line). It reads
	// the account's choice when it mounts (with the first answer).
	import { onMount } from 'svelte';
	import { FOCUS_RING } from '$lib/ui/classes';
	import { m } from '$lib/paraglide/messages.js';
	import { grouping } from './grouping.svelte';

	interface Props {
		on: boolean;
		/** The id of the line saying why the rows came ungrouped, while it shows. */
		describedby?: string;
		class?: string;
		onchange: (on: boolean) => void;
	}
	let { on, describedby, class: className = '', onchange }: Props = $props();
	// The account's choice, read once per visit: it applies from the next search (grouping.svelte.ts).
	onMount(() => void grouping.load());
</script>

<!-- The Toggle primitive's markup (ui/fields/Toggle.svelte), without its two-way binding: the URL
     owns the value, and the binding would bring Svelte's bind runtime into the toolbar. -->
<label
	class="inline-flex cursor-pointer items-center gap-2 text-body pointer-coarse:min-h-(--target-min) {className}"
>
	<input
		type="checkbox"
		role="switch"
		class="toggle border-field-border {FOCUS_RING}"
		checked={on}
		data-testid="group-versions"
		aria-describedby={describedby}
		onchange={(e) => onchange(e.currentTarget.checked)}
	/>
	<span>{m.search_collapse_toggle()}</span>
</label>
