<script lang="ts">
	// The shared body of the hand-built connection screens (§10.10.8): a heading, the form's code
	// and the schema's fields for `sections`, loaded together when the screen opens. The form is
	// the screen's own form (`children`), because each adds its tests, clients or dialogs to the
	// engine's fields. A load failure shows the shell's usual banner.
	import type { Snippet } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { loadSettingsIndex } from '../form/schema';
	import type { FieldSpec } from '../form/types';

	interface Props {
		title: string;
		purpose: string;
		sections: string[];
		/** The screen's form, given the schema's fields. */
		children: Snippet<[fields: FieldSpec[]]>;
	}
	let { title, purpose, sections, children }: Props = $props();

	let fields = $state<FieldSpec[]>();
	let failed = $state(false);
	let attempt = $state(0);

	$effect(() => {
		void attempt;
		let live = true;
		failed = false;
		void loadSettingsIndex().then(
			(index) => {
				if (live) fields = index.fieldsOf(sections);
			},
			() => {
				if (live) failed = true;
			}
		);
		return () => {
			live = false;
		};
	});
</script>

<h1 class="text-title font-semibold">{title}</h1>
<p class="text-muted-text">{purpose}</p>
{#if failed}
	<Banner kind="error"
		>{m.settings_form_load_error()}{#snippet action()}<Button dense onclick={() => attempt++}
				>{m.settings_form_retry()}</Button
			>{/snippet}</Banner
	>
{:else if !fields}
	<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
{:else if !fields.length}
	<p>{m.settings_shell_server_unavailable()}</p>
{:else}
	{@render children(fields)}
{/if}
