<script lang="ts">
	// The watch folder's fields on the form engine, so saving, conflicts, the draft and an
	// environment lock work as on every other screen, with what the save can't say: the server
	// takes a folder inside Lodestar's data folder, then never reads it (`Section::check` doesn't
	// see the data folder), so the reason shows beside the field. The engine comes through its one
	// door (`form/embed`), as the section form and the wizard take it.
	import { onDestroy, tick, untrack } from 'svelte';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import IconWarning from '~icons/ph/warning-bold';
	import { FOCUS_RING } from '$lib/ui/classes';
	import {
		SettingField,
		SettingsForm,
		createSettingsForm,
		focusSettingField,
		groupFields,
		loadControls,
		loadMessages,
		messagesLoaded,
		settingText,
		widgetFor,
		type FieldKey,
		type FieldSpec
	} from '$lib/features/settings/form/embed';
	import { dataDirProblem } from './data-dir';

	let { fields, dataDir }: { fields: FieldSpec[]; dataDir: string } = $props();

	const form = untrack(() =>
		createSettingsForm({
			id: 'import',
			fields,
			href: (key) => resolve(`/settings/import#${encodeURIComponent(key)}` as '/search')
		})
	);
	onDestroy(() => form.destroy());
	const groups = $derived(groupFields(form.specs));

	// A link to a field (the URL hash, Copy link, the error summary) focuses it once its labels
	// are in; a lazy control loads first.
	async function reveal(key: FieldKey) {
		const spec = form.specs.find((spec) => spec.key === key);
		if (!spec) return;
		await loadControls([widgetFor(spec)]);
		await tick();
	}
	const sections = untrack(() => fields.map((spec) => spec.section));
	const inPlace = messagesLoaded(sections);
	let labelsReady = $state(inPlace);
	if (!inPlace) void loadMessages(sections).then(() => (labelsReady = true));
	const ready = $derived(labelsReady && form.status !== 'loading' && form.status !== 'load-error');
	let focusGeneration = 0;
	$effect(() => {
		const hash = page.url.hash;
		const generation = ++focusGeneration;
		if (!hash || !ready) return;
		let key: FieldKey;
		try {
			key = decodeURIComponent(hash.slice(1)) as FieldKey;
		} catch {
			return;
		}
		if (!form.specs.some((spec) => spec.key === key)) return;
		void untrack(() => reveal(key)).then(() => {
			if (generation === focusGeneration) focusSettingField(key);
		});
		return () => {
			++focusGeneration;
		};
	});

	const problem = $derived.by(() => {
		const folder = form.field('watch.folder').value;
		if (typeof folder !== 'string' || !dataDir) return '';
		const p = dataDirProblem(folder, dataDir);
		return p === 'inside' ? m.upload_watch_inside() : p === 'holds' ? m.upload_watch_holds() : '';
	});
</script>

<SettingsForm {form} onReveal={reveal}>
	{#each groups as group (group.group)}
		<section class="flex flex-col gap-6" aria-label={settingText(group.labelKey) ?? group.group}>
			<h2 class="text-body-lg font-semibold">{settingText(group.labelKey) ?? group.group}</h2>
			<!-- Two fields: an Advanced disclosure would hide half the section, so all show. -->
			{#each [...group.essentials, ...group.advanced] as spec (spec.key)}<SettingField
					{form}
					key={spec.key}
				/>{/each}
			{#if group.group === 'watch'}
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
		</section>
	{/each}
</SettingsForm>
