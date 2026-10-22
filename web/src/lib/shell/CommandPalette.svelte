<script lang="ts">
	// Command palette (⌘K / Ctrl+K): a combobox over a listbox inside a dialog (§10.13). Typing
	// filters, ↑/↓ move, Enter runs, Esc closes. Commands come from the keyboard registry, so
	// features add theirs by registering them.
	import { tick } from 'svelte';
	import { filterCommands, formatCombo, keys } from '$lib/keys';
	import { m } from '$lib/paraglide/messages.js';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Kbd from '$lib/ui/labels/Kbd.svelte';

	let { open = $bindable(false) }: { open?: boolean } = $props();

	let query = $state('');
	let selected = $state(0);
	let wrap: HTMLDivElement | undefined = $state();

	const available = $derived(keys.list().filter((c) => c.palette && c.active));
	const results = $derived(filterCommands(available, query));
	const active = $derived(Math.min(selected, results.length - 1));

	$effect(() => {
		if (!open) return;
		query = '';
		selected = 0;
	});

	// Bits focuses the first control (the close button) when the dialog opens; the input is the
	// one thing a palette is for.
	$effect(() => {
		if (!open || !wrap) return;
		const input = wrap.querySelector('input');
		const frame = requestAnimationFrame(() => input?.focus());
		return () => cancelAnimationFrame(frame);
	});

	async function run(id: string) {
		open = false;
		await tick(); // let the dialog restore focus first, so a command may move it
		keys.run(id);
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			e.preventDefault();
			if (!results.length) return;
			const step = e.key === 'ArrowDown' ? 1 : -1;
			selected = (active + step + results.length) % results.length;
			queueMicrotask(() =>
				document.getElementById(`palette-option-${selected}`)?.scrollIntoView({ block: 'nearest' })
			);
		} else if (e.key === 'Enter' && results[active]) {
			e.preventDefault();
			void run(results[active].id);
		}
	}
</script>

<Dialog bind:open title={m.shell_palette_title()} closeLabel={m.shell_palette_close()}>
	<div bind:this={wrap} class="flex flex-col gap-3">
		<Input
			role="combobox"
			aria-expanded="true"
			aria-controls={results.length ? 'palette-list' : undefined}
			aria-autocomplete="list"
			aria-activedescendant={results.length ? `palette-option-${active}` : undefined}
			aria-label={m.shell_palette_input()}
			placeholder={m.shell_palette_input()}
			autocomplete="off"
			spellcheck="false"
			bind:value={query}
			{onkeydown}
			data-testid="palette-input"
		/>
		{#if results.length}
			<ul
				id="palette-list"
				role="listbox"
				aria-label={m.shell_palette_listbox()}
				class="max-h-(--listbox-max-height) overflow-y-auto"
			>
				{#each results as command, i (command.id)}
					<!-- Keyboard users drive the options from the input (aria-activedescendant). -->
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<li
						id="palette-option-{i}"
						role="option"
						aria-selected={i === active}
						data-testid="palette-option"
						class="flex min-h-(--control-height) cursor-default items-center justify-between gap-3 rounded-field px-3 text-body pointer-coarse:min-h-(--target-min) {i ===
						active
							? 'bg-selected'
							: 'hover:bg-hover'}"
						onclick={() => run(command.id)}
						onpointermove={() => (selected = i)}
					>
						<span class="min-w-0 truncate">{command.title}</span>
						{#if command.keys.length}
							<Kbd>{formatCombo(command.keys[0]!)}</Kbd>
						{:else if command.section}
							<span class="text-caption {i === active ? '' : 'text-muted-text'}"
								>{command.section}</span
							>
						{/if}
					</li>
				{/each}
			</ul>
		{:else}
			<p class="px-3 text-body text-muted-text" aria-live="polite">
				{m.shell_palette_empty({ query: query.trim() })}
			</p>
		{/if}
	</div>
</Dialog>
