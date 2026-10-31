<script lang="ts">
	// The one registered control of the connection screens: it loads the widget of its field
	// (`WIDGETS`) when the field is on the page, so the cards and checkboxes stay out of the
	// screen's first view. A placeholder of the same name holds the place.
	import type { WidgetProps } from '../form/widgets';
	import Lazy from './Lazy.svelte';
	import { WIDGETS } from './widgets';

	let props: WidgetProps = $props();
	const load = $derived(WIDGETS[props.form.field(props.key).spec.name]);
</script>

{#if load}
	{#key props.key}<Lazy {load} props={{ ...props }} />{/key}
{/if}
