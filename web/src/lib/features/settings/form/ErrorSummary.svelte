<script lang="ts">
	// The error summary (§10.10.5), shown after a save that failed with errors: each links to its
	// field. Loaded by `SettingsForm.svelte` when it first shows. It takes focus once per failed
	// save, never because it came back while the user fixes a field (an error that returns).
	import { m } from '$lib/paraglide/messages.js';
	import { tick, untrack } from 'svelte';
	import type { SettingsForm } from './form.svelte';
	import { fieldLabel } from './messages';
	import type { FieldKey } from './types';

	interface Props {
		form: SettingsForm;
		/** Changes with each failed save: the heading takes focus again. */
		focusId: number;
		/** The last `focusId` it took focus for: mounting again doesn't take it a second time. */
		focused: number;
		onfocused: (id: number) => void;
		onReveal?: (key: FieldKey) => void | Promise<void>;
	}

	let { form, focusId, focused, onfocused, onReveal }: Props = $props();

	let heading = $state<HTMLElement>();
	$effect(() => {
		const id = focusId;
		untrack(() => {
			if (id === focused) return;
			heading?.focus();
			onfocused(id);
		});
	});

	const CONTROL_SELECTORS = [
		'input:checked',
		'[role="radio"][aria-checked="true"]',
		'input, select, textarea, [role="radio"], button'
	];

	async function reveal(key: FieldKey) {
		await onReveal?.(key);
		await tick();
		// The field's control, never its header (the ⋯ menu): the label's target, else the checked
		// option, else the first control inside the field's control area.
		const root = document.getElementById(key);
		const area = root?.querySelector<HTMLElement>('[data-control]');
		const label = root?.querySelector<HTMLLabelElement>('label[for]');
		const control =
			(label?.htmlFor ? document.getElementById(label.htmlFor) : null) ??
			// In order of preference (a selector list would match in document order instead).
			CONTROL_SELECTORS.map((q) => area?.querySelector<HTMLElement>(q)).find(Boolean);
		root?.scrollIntoView?.({ block: 'center' });
		control?.focus();
	}
</script>

<div class="rounded-box border border-error p-4" data-testid="error-summary">
	<h2 bind:this={heading} tabindex="-1" class="text-title font-semibold">
		{m.settings_form_summary_title()}
	</h2>
	<ul class="mt-2 flex flex-col gap-1">
		{#each form.errors as e, i (e.key ?? `form-${i}`)}
			<li>
				{#if e.key}
					{@const key = e.key}
					{@const spec = form.specs.find((s) => s.key === key)}
					<a
						class="link inline-flex items-center text-error pointer-coarse:min-h-(--target-min)"
						href="#{key}"
						onclick={(ev) => {
							ev.preventDefault();
							void reveal(key);
						}}>{spec ? `${fieldLabel(spec)}: ` : ''}{e.message}</a
					>
				{:else}
					<span class="text-error">{e.message}</span>
				{/if}
			</li>
		{/each}
	</ul>
</div>
