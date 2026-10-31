<script lang="ts">
	// The Verbosity preset (§10.10.12): a SegmentedControl in plain words with the technical term
	// ("Normal (info)"). It turns on every event at or above that level; the event list says when
	// single events differ from it (Custom).
	import { m } from '$lib/paraglide/messages.js';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import { fieldLabel } from '../form/messages';
	import type { WidgetProps } from '../form/widgets';
	import { VERBOSITY } from './rules';

	let { form, key, controlProps }: WidgetProps = $props();
	const f = $derived(form.field(key));
	const LABELS: Record<string, () => string> = {
		errors: m.settings_logging_verbosity_errors,
		warnings: m.settings_logging_verbosity_warnings,
		normal: m.settings_logging_verbosity_normal,
		detailed: m.settings_logging_verbosity_detailed,
		everything: m.settings_logging_verbosity_everything
	};
</script>

<div id={controlProps.id} aria-describedby={controlProps['aria-describedby']}>
	<SegmentedControl
		label={fieldLabel(f.spec)}
		options={VERBOSITY.map((value) => ({ value, label: LABELS[value]!(), disabled: f.locked }))}
		value={String(f.value ?? 'normal')}
		onchange={(v) => form.set(key, v)}
		class="flex-wrap"
	/>
</div>
