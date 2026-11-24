<script lang="ts">
	// The watch folder's settings (UP-1's `watch` section) on the form engine, with what the save
	// can't say: the server takes a folder inside Lodestar's data folder, then never reads it
	// (`Section::check` doesn't see the data folder), so the reason shows beside the field, read
	// from General's data folder. Loaded after the upload part paints; a load failure says so here.
	import { resolve } from '$app/paths';
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import IconWarning from '~icons/ph/warning-bold';
	import { FOCUS_RING } from '$lib/ui/classes';
	import GroupedForm from '$lib/features/settings/screens2/GroupedForm.svelte';
	import Lazy from '$lib/features/settings/screens2/Lazy.svelte';
	import { loadSettingsIndex } from '$lib/features/settings/form/schema';
	import type { FieldSpec } from '$lib/features/settings/form/types';
	import { dataDirProblem } from './upload';

	let fields = $state<FieldSpec[]>();
	let failed = $state(false);
	let attempt = $state(0);
	/** General's data folder, as the server shows it; empty until it's in (or if it can't be read). */
	let dataDir = $state('');

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

	const problemText = (folder: unknown) => {
		if (typeof folder !== 'string' || !dataDir) return '';
		const p = dataDirProblem(folder, dataDir);
		return p === 'inside' ? m.upload_watch_inside() : p === 'holds' ? m.upload_watch_holds() : '';
	};
</script>

{#if failed}
	<Lazy
		load={() => import('$lib/features/settings/screens2/LoadFailed.svelte')}
		props={{ onretry: () => attempt++ }}
	/>
{:else if !fields}
	<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
{:else if fields.length}
	<GroupedForm id="import" {fields}>
		{#snippet after(group, form)}
			{#if group === 'watch'}
				{@const problem = problemText(form.field('watch.folder').value)}
				<div class="flex flex-col gap-2 text-small" data-testid="watch-notes">
					{#if problem}
						<p class="flex items-start gap-1" role="status" data-testid="watch-problem">
							<IconWarning class="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
							<span>{problem}</span>
						</p>
					{/if}
					<p>{m.upload_watch_how()}</p>
					<p>{m.upload_watch_after()}</p>
					<p>{m.upload_watch_rule()}</p>
					<p>
						{m.upload_watch_logs()}
						<a class="link {FOCUS_RING}" href={resolve('/logs')} data-testid="watch-logs"
							>{m.upload_watch_logs_link()}</a
						>
					</p>
				</div>
			{/if}
		{/snippet}
	</GroupedForm>
{/if}
