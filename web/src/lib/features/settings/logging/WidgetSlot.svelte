<script lang="ts">
	// The Verbosity preset, the event list and the component levels stand in for the engine's
	// controls, and load when their field is on the page, so they stay out of the first view.
	import { untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { WidgetProps } from '../form/widgets';

	let props: WidgetProps = $props();
	const LOAD: Record<string, () => Promise<{ default: unknown }>> = {
		verbosity: () => import('./VerbosityWidget.svelte'),
		events: () => import('./EventList.svelte'),
		components: () => import('./ComponentLevels.svelte')
	};
	const name = untrack(() => props.form.field(props.key).spec.name);
	let Widget = $state.raw<typeof import('./EventList.svelte').default>();
	let failed = $state(false);
	LOAD[name]?.().then(
		(mod) => (Widget = mod.default as typeof Widget),
		() => (failed = true)
	);
</script>

{#if Widget}
	<Widget {...props} />
{:else if failed}
	<p role="alert" class="text-small text-error">{m.settings_form_chunk_failed()}</p>
{/if}
