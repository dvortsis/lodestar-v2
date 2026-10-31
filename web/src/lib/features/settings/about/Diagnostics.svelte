<script lang="ts">
	// The diagnostics block and "Report a problem" (§10.10.13, §12.1): the text is shown before
	// anything is copied or put in a link, so nothing identifying leaves without being seen.
	import { m } from '$lib/paraglide/messages.js';
	import { copyText } from '$lib/util/clipboard';
	import Button from '$lib/ui/controls/Button.svelte';
	import { diagnosticsText, problemUrl, repoBase, type System } from './logic';

	let { info, uiBuild }: { info: System; uiBuild: string } = $props();
	const text = $derived(diagnosticsText(info, uiBuild));
	const base = $derived(repoBase(info.repository));
	const url = $derived(problemUrl(base, text));
	let copied = $state<'copied' | 'manual'>();
	let reporting = $state(false);

	async function copy() {
		copied = await copyText(text);
	}
</script>

<pre
	class="rounded-box border border-field-border p-3 font-mono text-small whitespace-pre-wrap"
	data-testid="about-diagnostics">{text}</pre>
<div class="flex flex-wrap items-center gap-3">
	<Button onclick={copy}>{m.settings_about_copy_diagnostics()}</Button>
	{#if base}
		<Button onclick={() => (reporting = !reporting)} aria-expanded={reporting}
			>{m.settings_about_problem()}</Button
		>
	{/if}
	<span role="status" class="text-small text-muted-text"
		>{copied === 'copied'
			? m.settings_about_copied()
			: copied
				? m.settings_about_copy_manual()
				: ''}</span
	>
</div>
{#if reporting && base}
	<p class="text-muted-text">{m.settings_about_problem_lead()}</p>
	{#if !url}<p>{m.settings_about_problem_long()}</p>{/if}
	<!-- eslint-disable svelte/no-navigation-without-resolve -- an external https link to the build's repository (logic.repoBase) -->
	<a
		class="self-start underline"
		href={url ?? `${base}/issues/new`}
		target="_blank"
		rel="noopener noreferrer"
		onclick={() => (url ? undefined : void copyText(text))}
		>{m.settings_about_problem_open()}<span class="sr-only"> {m.settings_about_new_tab()}</span></a
	>
	<!-- eslint-enable svelte/no-navigation-without-resolve -->
{/if}
