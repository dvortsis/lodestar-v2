<script lang="ts">
	// Shown when the server runs another UI build than this tab (stale.ts). Quiet and polite: it
	// never takes focus and never reloads by itself. Reload goes through the leave guards, so a
	// Settings draft still gets the browser's own question.
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { hasUnsaved } from './stale';

	let unsaved = $state(hasUnsaved());
	// A polite live region is announced when its text changes, not when it is inserted holding the
	// text (NVDA and JAWS with Chrome usually stay silent). The banner mounts empty; the words
	// follow. Reload is there from the start.
	let ready = $state(false);
	$effect(() => {
		const t = setTimeout(() => (ready = true), 150);
		return () => clearTimeout(t);
	});
	// The answer changes with edits, Save, Discard and leaving a screen: ask again shortly after
	// any of them (one probe per burst, since kit saves page state on each).
	$effect(() => {
		let timer: ReturnType<typeof setTimeout> | undefined;
		const ask = () => {
			clearTimeout(timer);
			timer = setTimeout(() => (unsaved = hasUnsaved()), 250);
		};
		const events = ['input', 'change', 'focusout', 'click'] as const;
		for (const e of events) document.addEventListener(e, ask, true);
		return () => {
			clearTimeout(timer);
			for (const e of events) document.removeEventListener(e, ask, true);
		};
	});
</script>

<div class="m-4 mb-0" data-testid="stale-banner">
	<Banner>
		{#if ready}{m.shell_stale_text()}{#if unsaved}
				{m.shell_stale_unsaved()}{/if}{/if}
		{#snippet action()}
			<Button dense onclick={() => location.reload()}>{m.shell_stale_reload()}</Button>
		{/snippet}
	</Banner>
</div>
