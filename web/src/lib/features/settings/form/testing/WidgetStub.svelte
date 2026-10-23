<script lang="ts">
	// Tests only: a registered widget that shows what SettingField hands it.
	import type { WidgetProps } from '../widgets';

	let { key, controlProps }: WidgetProps = $props();
</script>

<span data-testid="widget-stub" data-key={key} id={controlProps.id}>stub</span>
