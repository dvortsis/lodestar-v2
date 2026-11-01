<script lang="ts">
	// The `time-zone` widget: a text input with the browser's IANA names as suggestions. Empty
	// follows the browser, and the placeholder says so. A name the browser doesn't know is shown
	// as an error and never reaches the form, so instant mode can't save "Foo/Bar" or a partial
	// name mid-typing; the server still has the final say.
	import { m } from '$lib/paraglide/messages.js';
	import Input from '$lib/ui/fields/Input.svelte';
	import type { WidgetProps } from '../form/widgets';

	let { form, key, controlProps, dense = false }: WidgetProps = $props();
	const f = $derived(form.field(key));
	const zones: string[] = (() => {
		try {
			return Intl.supportedValuesOf('timeZone');
		} catch {
			return [];
		}
	})();
	const listId = $derived(`${controlProps.id}-zones`);
	const errorId = $derived(`${controlProps.id}-zone-error`);

	function knownZone(name: string): boolean {
		if (name === '') return true;
		try {
			new Intl.DateTimeFormat(undefined, { timeZone: name });
			return true;
		} catch {
			return false;
		}
	}

	const saved = () => (typeof f.value === 'string' ? f.value : '');
	let text = $state(saved());
	let sent = saved();
	// A bad name is flagged (aria-invalid) as typed but announced (role="alert") only once the
	// field is left, so a screen reader doesn't speak on every key; once shown it stays until valid.
	let invalid = $state(false);
	let announced = $state(false);
	// An Undo, a conflict or another tab changing the value replaces what's typed.
	$effect(() => {
		const v = saved();
		if (v === sent) return;
		sent = v;
		text = v;
		invalid = false;
		announced = false;
	});
	function input(value: string) {
		text = value;
		invalid = !knownZone(value);
		if (!invalid) announced = false;
		if (invalid) return;
		sent = value;
		form.set(key, value);
	}
</script>

<Input
	{...controlProps}
	aria-describedby={[controlProps['aria-describedby'], invalid && announced ? errorId : undefined]
		.filter(Boolean)
		.join(' ') || undefined}
	aria-invalid={invalid || controlProps['aria-invalid'] ? true : undefined}
	{dense}
	list={listId}
	placeholder={m.setting_automatic()}
	value={text}
	disabled={f.locked}
	spellcheck={false}
	autocomplete="off"
	oninput={(e) => input(e.currentTarget.value)}
	onblur={() => {
		announced = invalid;
		form.blur(key);
	}}
/>
{#if invalid && announced}<p id={errorId} class="text-small text-error" role="alert">
		{m.settings_screen_time_zone_unknown()}
	</p>{/if}
<datalist id={listId}>
	{#each zones as zone (zone)}<option value={zone}></option>{/each}
</datalist>
