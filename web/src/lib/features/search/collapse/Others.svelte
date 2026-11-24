<script lang="ts">
	// The quiet "+3 other versions" disclosure on a grouped row (UI review 02 §5.1). A button
	// inside the row: pressing it opens or closes the group without opening the row (VirtualList
	// treats a press on a control as a control's). → and ← do the same from the row. It comes with
	// the results panel, so it paints with the first grouped rows; what opening needs loads then.
	import CaretRight from '~icons/ph/caret-right-bold';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { othersText } from './others';
	import './others.css';

	interface Props {
		others: number;
		open: boolean;
		testid: string;
		ontoggle: () => void;
	}
	let { others, open, testid, ontoggle }: Props = $props();
</script>

<button
	type="button"
	class="cl-others {FOCUS_RING} {TARGET_MIN}"
	tabindex="-1"
	aria-expanded={open}
	data-testid="{testid}-others"
	onclick={ontoggle}><CaretRight aria-hidden="true" />{othersText(others, currentLocale())}</button
>
