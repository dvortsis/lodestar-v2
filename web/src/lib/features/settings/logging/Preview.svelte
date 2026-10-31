<script lang="ts">
	// The preview (§10.10.12, §12.1): example lines for the staged events and data kinds, rendered by
	// the server from sample data (never real torrents), and the estimated volume per day for each
	// output, with how many days the files would hold. It follows the staged values through
	// `POST /settings/preview`, before anything is saved.
	import { api, isAbort, latest } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { formatBytes, formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import type { SettingsForm } from '../form';

	let { form }: { form: SettingsForm } = $props();

	type Example = { event: string; console: string | null; file: string | null };
	interface Shown {
		console?: number;
		file?: number;
		days?: number | null;
		examples: Example[];
	}
	const MIB = 1024 * 1024;
	const DEBOUNCE_MS = 400;

	// The staged values the preview depends on (Verbosity always, so the section has one field).
	const body = $derived.by(() => {
		const staged: Record<string, unknown> = {
			verbosity: form.field('logging.verbosity').value
		};
		for (const s of form.specs)
			if (s.preview && form.field(s.key).dirty) staged[s.name] = form.field(s.key).value;
		return JSON.stringify({ logging: staged });
	});

	let shown = $state.raw<Shown>();
	let failed = $state(false);
	const slot = latest();
	$effect(() => {
		const batch = body;
		const timer = setTimeout(() => {
			slot
				.run((signal) =>
					api.post('/settings/preview', {
						body: JSON.parse(batch) as components['schemas']['SettingsBatch'],
						signal
					})
				)
				.then(
					(p) => {
						const est = p.sections.logging?.fields.verbosity?.estimates ?? [];
						const of = (n: string) => est.find((e) => e.name === n)?.value;
						const num = (v: unknown) => (typeof v === 'number' ? v : undefined);
						shown = {
							console: num(of('console_mib_per_day')),
							file: num(of('file_mib_per_day')),
							days: num(of('file_days_kept')) ?? null,
							examples: Array.isArray(of('examples')) ? (of('examples') as Example[]) : []
						};
						failed = false;
					},
					(e) => {
						if (!isAbort(e)) failed = true;
					}
				);
		}, DEBOUNCE_MS);
		return () => clearTimeout(timer);
	});
	$effect(() => () => slot.cancel());
</script>

<section class="flex flex-col gap-3" aria-labelledby="logging-preview" aria-live="polite">
	<h3 id="logging-preview" class="text-body font-semibold">{m.settings_logging_preview()}</h3>
	{#if failed}
		<p class="text-small text-muted-text">{m.settings_logging_preview_failed()}</p>
	{:else if !shown}
		<p aria-busy="true" class="text-small text-muted-text">{m.settings_form_loading()}</p>
	{:else}
		<ul class="flex flex-col gap-1 text-small">
			{#if shown.console !== undefined}<li data-testid="log-volume-console">
					{m.settings_logging_volume_console({ size: formatBytes(shown.console * MIB) })}
				</li>{/if}
			{#if shown.file !== undefined}<li data-testid="log-volume-file">
					{m.settings_logging_volume_file({ size: formatBytes(shown.file * MIB) })}
					{#if shown.days !== null}{m.settings_logging_volume_days({
							days: formatCount(shown.days ?? null)
						})}{/if}
				</li>{/if}
		</ul>
		{#if shown.examples.length}
			<p class="text-small text-muted-text">{m.settings_logging_preview_examples()}</p>
			<ul class="flex flex-col gap-2" data-testid="log-examples">
				{#each shown.examples as x, i (i)}
					<li class="text-caption">
						<Mono wrap>{x.file ?? x.console}</Mono>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</section>
