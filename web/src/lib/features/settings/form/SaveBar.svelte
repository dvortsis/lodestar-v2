<script lang="ts">
	// The pinned save bar (§10.10.5): what's unsaved or just saved, Discard and Save, or Undo.
	import Button from '$lib/ui/controls/Button.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconRestart from '~icons/ph/arrows-clockwise';
	import IconCheck from '~icons/ph/check-circle';
	import type { SettingsForm } from './form.svelte';
	import { askDiscard } from './questions';

	interface Props {
		form: SettingsForm;
		/** Closes questions this bar asked (the form's own signal). */
		signal: AbortSignal;
		ondiscard: () => void;
	}

	let { form, signal, ondiscard }: Props = $props();

	// The bar never covers the focused field (§10.10.5): a field focused behind it scrolls clear,
	// with the ramp's 4 (16 px at the default root size) of air.
	let el = $state<HTMLElement>();
	$effect(() => {
		const root = el?.parentElement;
		if (!el || !root) return;
		const bar = el;
		function onfocusin(e: FocusEvent) {
			if (!(e.target instanceof HTMLElement) || bar.contains(e.target)) return;
			const air = parseFloat(getComputedStyle(document.documentElement).fontSize);
			const hidden =
				e.target.getBoundingClientRect().bottom + air - bar.getBoundingClientRect().top;
			if (hidden > 0) window.scrollBy({ top: hidden });
		}
		root.addEventListener('focusin', onfocusin);
		return () => root.removeEventListener('focusin', onfocusin);
	});

	const unsaved = $derived(form.dirtyCount > 0);
	const count = $derived(formatCount(form.dirtyCount, currentLocale()));

	async function discard() {
		// One change is cheap to redo; more than one asks first.
		if (form.dirtyCount > 1 && (await askDiscard(count, signal)) !== 'discard') return;
		form.discard();
		ondiscard();
	}
</script>

<div
	class="ls-bar sticky bottom-0 z-10 flex flex-wrap items-center justify-between gap-3 border-t border-base-300 bg-base-100 py-2"
	data-testid="save-bar"
	bind:this={el}
>
	<p class="flex flex-wrap items-center gap-2 text-body" data-testid="save-bar-status">
		{#if form.status === 'saving'}
			{m.settings_form_saving()}
		{:else if unsaved}
			{form.dirtyCount === 1 ? m.settings_form_unsaved_one() : m.settings_form_unsaved({ count })}
		{:else if form.status === 'saved' || form.undoUntil !== null}
			<span class="inline-flex items-center gap-1">
				{#if form.restartPending.length}
					<IconRestart class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				{:else}
					<IconCheck class="size-(--icon-size-row) shrink-0 text-success" aria-hidden="true" />
				{/if}
				{form.restartPending.length ? m.settings_form_saved_restart() : m.settings_form_saved()}
			</span>
			{#if form.undoSkipsSecrets}
				<span class="text-small text-muted-text">{m.settings_form_undo_secrets()}</span>
			{/if}
		{/if}
	</p>
	<div class="flex items-center gap-2">
		{#if unsaved}
			<Button onclick={discard}>{m.settings_form_discard()}</Button>
			<!-- Never disabled: with errors, Save shows the summary (§10.10.5). -->
			<Button variant="primary" loading={form.status === 'saving'} onclick={() => void form.save()}
				>{m.settings_form_save()}</Button
			>
		{:else if form.undoUntil !== null}
			<Button onclick={() => void form.undo()}>{m.settings_form_undo()}</Button>
		{/if}
	</div>
</div>
