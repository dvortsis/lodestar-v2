<script lang="ts">
	// The watch folder's settings (UP-1's `watch` section), loaded after the upload part paints:
	// the section's fields from the schema, General's data folder for the hint, and the form
	// (`WatchForm`) once the fields are in.
	// Nothing here imports GroupedForm, Lazy or the form's shared status lines: Rolldown groups
	// modules by the entries that reach them, so reaching only a part of those chunks would split
	// them and grow every Settings first view (form/embed.ts).
	import type { Component } from 'svelte';
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import { loadSettingsIndex } from '$lib/features/settings/form/schema';
	import type { FieldSpec } from '$lib/features/settings/form/embed';
	import WatchForm from './WatchForm.svelte';

	let fields = $state<FieldSpec[]>();
	let failed = $state(false);
	let attempt = $state(0);
	/** General's data folder, as the server shows it; empty until it's in (or if it can't be read). */
	let dataDir = $state('');
	// The shell's usual banner with Retry, loaded only when the fields can't be read.
	let LoadFailed = $state<Component<{ onretry: () => void }>>();

	$effect(() => {
		void attempt;
		let live = true;
		failed = false;
		void loadSettingsIndex().then(
			(index) => {
				if (live) fields = index.fieldsOf(['watch']);
			},
			() => {
				if (live) failed = true;
			}
		);
		// Only for the hint: without it the field still works, and the server still refuses to read.
		void api.get('/settings/{section}', { path: { section: 'general' } }).then(
			(general) => {
				const value = general.fields?.data_dir?.value;
				if (live && typeof value === 'string') dataDir = value;
			},
			() => {}
		);
		return () => {
			live = false;
		};
	});
	$effect(() => {
		if (failed && !LoadFailed)
			void import('$lib/features/settings/screens2/LoadFailed.svelte').then(
				(mod) => (LoadFailed = mod.default),
				() => {}
			);
	});
</script>

{#if failed}
	{#if LoadFailed}<LoadFailed onretry={() => attempt++} />{/if}
{:else if !fields}
	<p aria-busy="true" role="status">{m.upload_watch_loading()}</p>
{:else if fields.length}
	<WatchForm {fields} {dataDir} />
{/if}
