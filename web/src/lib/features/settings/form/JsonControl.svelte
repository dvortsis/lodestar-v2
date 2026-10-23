<script lang="ts">
	// The fallback for a shape the form can't read (§10.10.4), so a new server setting still
	// shows up: its value as JSON, staged once it parses.
	import Textarea from '$lib/ui/fields/Textarea.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { JsonDraft } from './values';
	import type { SettingsForm } from './form.svelte';
	import type { FieldKey } from './types';
	import type { FieldControlProps } from './widgets';

	interface Props {
		form: SettingsForm;
		key: FieldKey;
		controlProps: FieldControlProps;
	}

	let { form, key, controlProps }: Props = $props();

	const f = $derived(form.field(key));
	const errorId = $props.id();
	const pretty = (v: unknown) => (v === undefined ? '' : JSON.stringify(v, null, 2));

	let text = $state('');
	// Text that doesn't parse is staged as a `JsonDraft`: dirty and invalid until it parses.
	const invalid = $derived(f.value instanceof JsonDraft);
	// Follow the form's value unless what's typed already says the same.
	$effect(() => {
		const v = f.value;
		if (v instanceof JsonDraft) text = v.text;
		else if (!same(text, v)) text = pretty(v);
	});

	function same(s: string, v: unknown): boolean {
		try {
			return JSON.stringify(JSON.parse(s)) === JSON.stringify(v);
		} catch {
			return false;
		}
	}

	function input(s: string) {
		text = s;
		let parsed: unknown;
		try {
			parsed = JSON.parse(s);
		} catch {
			form.set(key, new JsonDraft(s));
			return;
		}
		form.set(key, parsed);
	}
</script>

<Textarea
	{...controlProps}
	mono
	rows={4}
	value={text}
	disabled={f.locked}
	spellcheck={false}
	aria-invalid={invalid || controlProps['aria-invalid']}
	aria-describedby={[controlProps['aria-describedby'], invalid && !f.error ? errorId : '']
		.filter(Boolean)
		.join(' ') || undefined}
	oninput={(e) => input(e.currentTarget.value)}
	onblur={() => form.blur(key)}
/>
{#if invalid && !f.error}
	<p id={errorId} class="text-small text-error">{m.settings_form_json_invalid()}</p>
{/if}
