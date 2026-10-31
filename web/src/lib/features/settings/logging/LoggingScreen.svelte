<script lang="ts">
	// Settings → Logging (§10.10.12, §12.1): what gets a line (events), what a line may contain
	// (data kinds and the opt-ins) and what is never logged. The events' settings go through the
	// form engine. What the server has on now (`GET /logs/catalog`) drives the banners, the event
	// list and the data kinds, which load on demand with the dialogs and the privacy requests.
	import { setContext } from 'svelte';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { loadSettingsIndex } from '../form/schema';
	import type { FieldSpec } from '../form/types';
	import type { SettingsForm } from '../form';
	import LoggingForm from './LoggingForm.svelte';
	import type { CatalogHolder, CatalogKey } from './context';

	/** The fields this screen's form shows; the data kinds and opt-ins are sent on their own. */
	const FORM_FIELDS = ['verbosity', 'events', 'components', 'libraries'];

	let parts = $state.raw<typeof import('./parts')>();
	let catalog = $state.raw<import('./catalog.svelte').LoggingCatalog>();
	let partsFailed = $state(false);
	setContext<CatalogHolder>('logging-catalog' satisfies CatalogKey, {
		get catalog() {
			return catalog;
		}
	});
	$effect(() => {
		let stop: (() => void) | undefined;
		let live = true;
		import('./parts').then(
			(p) => {
				if (!live) return;
				parts = p;
				catalog = new p.LoggingCatalog();
				stop = catalog.start();
			},
			() => (partsFailed = true)
		);
		return () => {
			live = false;
			stop?.();
		};
	});

	let fields = $state<FieldSpec[]>();
	let failed = $state(false);
	let attempt = $state(0);
	let form = $state.raw<SettingsForm>();
	$effect(() => {
		void attempt;
		let live = true;
		failed = false;
		void loadSettingsIndex().then(
			(index) => {
				if (live) fields = index.fieldsOf(['logging']).filter((f) => FORM_FIELDS.includes(f.name));
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

<h1 class="text-title font-semibold">{m.settings_logging_title()}</h1>
<p class="text-muted-text">{m.settings_logging_purpose()}</p>
<a class="self-start underline" href={resolve('/logs')}>{m.settings_logging_open_live()}</a>
{#if parts && catalog}
	<parts.Banners {catalog} {form} />
{:else if partsFailed}
	<p role="alert" class="text-small text-error">{m.settings_form_chunk_failed()}</p>
{/if}

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
	<LoggingForm {fields} onform={(f) => (form = f)} />
{/if}
{#if parts && catalog}<parts.DataKinds {catalog} {form} />{/if}
