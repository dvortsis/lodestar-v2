<script lang="ts">
	// Settings → Preferences → Language & region (§10.10.8): the `region` group of `preferences`,
	// shown by the shell's section form. The rest of `preferences` belongs to Appearance and
	// Keyboard. This body stays small: the form and the engine are the shell's on-demand chunk.
	import type { Component, ComponentProps } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { loadSettingsIndex } from '../form/schema';
	import type { FieldSpec } from '../form/types';
	import { screens } from './screen';

	type Form = Component<ComponentProps<typeof import('../shell/SectionForm.svelte').default>>;
	let fields = $state<FieldSpec[]>();
	let SectionForm = $state<Form>();
	let failed = $state(false);
	let chunkFailed = $state(false);
	let attempt = $state(0);

	$effect(() => {
		void attempt;
		let live = true;
		failed = false;
		void import('../shell/SectionForm.svelte').then(
			(mod) => {
				if (live) SectionForm = mod.default;
			},
			() => {
				if (live) chunkFailed = true;
			}
		);
		void loadSettingsIndex().then(
			(index) => {
				if (live)
					fields = index.fieldsOf(['preferences']).filter((spec) => spec.group === 'region');
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

<h1 class="text-title font-semibold">{m.settings_language_title()}</h1>
<p class="text-muted-text">{m.settings_language_purpose()}</p>
{#if failed}
	<Banner kind="error"
		>{m.settings_form_load_error()}{#snippet action()}<Button dense onclick={() => attempt++}
				>{m.settings_form_retry()}</Button
			>{/snippet}</Banner
	>
{:else if chunkFailed}
	<Banner kind="error"
		>{m.settings_form_chunk_failed()}{#snippet action()}<Button
				dense
				onclick={() => location.reload()}>{m.settings_form_reload()}</Button
			>{/snippet}</Banner
	>
{:else if !fields || !SectionForm}
	<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
{:else if !fields.length}
	<p>{m.settings_shell_server_unavailable()}</p>
{:else}
	<SectionForm screen={screens[0]!} {fields} />
{/if}
