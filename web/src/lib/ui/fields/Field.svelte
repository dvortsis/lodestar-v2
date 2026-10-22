<script lang="ts" module>
	/** What a Field hands its control: spread it onto the Input, Select, Textarea, etc. */
	export interface FieldControlProps {
		id: string;
		'aria-describedby': string | undefined;
		'aria-invalid': true | undefined;
		'aria-required': true | undefined;
		/** The visible label's id (for popups that need their own name, e.g. a Combobox list). */
		'aria-labelledby': string | undefined;
	}
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import IconWarning from '~icons/ph/warning-circle';

	interface Props {
		/** Visible label. Omit when the control carries its own (Checkbox, Radio, Toggle). */
		label?: string;
		help?: string;
		/** Inline validation message; its presence marks the control invalid. */
		error?: string;
		required?: boolean;
		class?: string;
		children: Snippet<[FieldControlProps]>;
	}

	let { label, help, error, required = false, class: className = '', children }: Props = $props();

	const uid = $props.id();
	const id = `${uid}-control`;
	const helpId = `${uid}-help`;
	const errorId = `${uid}-error`;
	const labelId = `${uid}-label`;

	const describedby = $derived(
		[help ? helpId : '', error ? errorId : ''].filter(Boolean).join(' ') || undefined
	);
</script>

<div class="flex flex-col gap-1 {className}">
	{#if label}
		<label id={labelId} for={id} class="text-small font-medium text-base-content">
			{label}
			<!-- The input carries aria-required, so the marker is visual only (announced once). -->
			{#if required}<span aria-hidden="true" class="text-error">*</span>{/if}
		</label>
	{/if}
	{@render children({
		id,
		'aria-describedby': describedby,
		'aria-invalid': error ? true : undefined,
		'aria-required': required ? true : undefined,
		'aria-labelledby': label ? labelId : undefined
	})}
	{#if help}
		<p id={helpId} class="text-small text-muted-text">{help}</p>
	{/if}
	{#if error}
		<p
			id={errorId}
			class="flex items-start gap-1 text-small text-base-content"
			data-testid="field-error"
		>
			<IconWarning class="mt-0.5 size-4 shrink-0 text-error" aria-hidden="true" />
			<span>{error}</span>
		</p>
	{/if}
</div>
