<script lang="ts" module>
	export interface ComboboxOption {
		value: string;
		label: string;
		disabled?: boolean;
	}
</script>

<script lang="ts">
	import { FOCUS_RING, TARGET_MIN } from '../classes';
	import { m } from '$lib/paraglide/messages.js';

	// WAI-ARIA 1.2 combobox with a listbox popup and list autocomplete. Focus stays on the input;
	// the active option is announced through aria-activedescendant. This is the base the query bar
	// builds on (§10.4), so `value` is free text and `onselect` fires on choosing an option.
	interface Props {
		options: ComboboxOption[];
		/** The input text (free text; typing filters the list unless `filter` is false). */
		value?: string;
		/** Accessible name; use Field's label instead and pass `id` + the Field props. */
		label?: string;
		id?: string;
		placeholder?: string;
		/** Filter `options` by the typed text. Turn off when the caller supplies matching options. */
		filter?: boolean;
		dense?: boolean;
		/** Empty-list text; defaults to the shared "No matches". */
		emptyLabel?: string;
		onselect?: (option: ComboboxOption) => void;
		name?: string;
		disabled?: boolean;
		/** The Field label's id; also names the popup list. */
		'aria-labelledby'?: string;
		'aria-describedby'?: string;
		'aria-invalid'?: true;
		'aria-required'?: true;
		class?: string;
	}

	let {
		options,
		value = $bindable(''),
		label,
		id,
		placeholder,
		filter = true,
		dense = false,
		emptyLabel,
		name,
		disabled = false,
		onselect,
		class: className = '',
		...aria
	}: Props = $props();

	const uid = $props.id();
	const inputId = $derived(id ?? `${uid}-input`);
	const listId = `${uid}-list`;

	let open = $state(false);
	let active = $state(-1);

	const shown = $derived(
		filter && value
			? options.filter((o) => o.label.toLowerCase().includes(value.toLowerCase()))
			: options
	);

	// Stale highlight would point at a different option once the list changes.
	$effect(() => {
		void shown;
		active = -1;
	});

	function choose(option: ComboboxOption | undefined) {
		if (!option || option.disabled) return;
		value = option.label;
		open = false;
		active = -1;
		onselect?.(option);
	}

	function move(delta: number) {
		const enabled = shown.flatMap((o, i) => (o.disabled ? [] : [i]));
		if (!enabled.length) return;
		const at = enabled.indexOf(active);
		active =
			at === -1
				? delta > 0
					? enabled[0]!
					: enabled[enabled.length - 1]!
				: enabled[(at + delta + enabled.length) % enabled.length]!;
	}

	function onkeydown(event: KeyboardEvent) {
		switch (event.key) {
			case 'ArrowDown':
				event.preventDefault();
				if (!open) open = true;
				move(1);
				break;
			case 'ArrowUp':
				event.preventDefault();
				if (!open) open = true;
				move(-1);
				break;
			case 'Enter':
				if (open && active >= 0) {
					event.preventDefault();
					choose(shown[active]);
				}
				break;
			case 'Escape':
				if (open) {
					event.preventDefault();
					open = false;
					active = -1;
				}
				break;
			case 'Home':
			case 'End':
				// Left to the text field (caret movement).
				break;
		}
	}
</script>

<div class="relative {className}">
	<input
		id={inputId}
		type="text"
		role="combobox"
		autocomplete="off"
		{name}
		{disabled}
		aria-label={label}
		aria-expanded={open}
		aria-controls={listId}
		aria-autocomplete="list"
		aria-activedescendant={open && active >= 0 ? `${uid}-opt-${active}` : undefined}
		{placeholder}
		bind:value
		class="input w-full border-field-border aria-invalid:border-error {dense
			? 'input-sm'
			: ''} {FOCUS_RING} {TARGET_MIN}"
		oninput={() => {
			open = true;
			active = -1;
		}}
		onblur={() => {
			open = false;
			active = -1;
		}}
		{onkeydown}
		{...aria}
	/>
	<ul
		id={listId}
		role="listbox"
		aria-label={aria['aria-labelledby'] ? undefined : label}
		aria-labelledby={aria['aria-labelledby']}
		hidden={!open}
		style:--combobox-max-height="calc(var(--row-height-comfortable) * 5)"
		onmousedown={(e) => e.preventDefault()}
		class="absolute z-10 mt-1 max-h-(--combobox-max-height) w-full overflow-auto rounded-box border border-base-300 bg-base-100 p-1"
	>
		{#each shown as option, i (option.value)}
			<!-- mousedown keeps focus on the input; click would arrive after blur closed the list. -->
			<li
				id="{uid}-opt-{i}"
				role="option"
				aria-selected={i === active}
				aria-disabled={option.disabled || undefined}
				class="cursor-pointer rounded-field px-3 py-2 text-body pointer-coarse:min-h-(--target-min) {i ===
				active
					? 'bg-selected'
					: 'hover:bg-hover'} {option.disabled ? 'opacity-60' : ''}"
				onmousedown={(e) => {
					e.preventDefault();
					choose(option);
				}}
			>
				{option.label}
			</li>
		{:else}
			<li
				role="option"
				aria-disabled="true"
				aria-selected="false"
				class="px-3 py-2 text-body text-muted-text"
			>
				{emptyLabel ?? m.common_no_matches()}
			</li>
		{/each}
	</ul>
</div>
