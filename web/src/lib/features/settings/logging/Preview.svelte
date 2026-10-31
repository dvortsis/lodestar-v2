<script lang="ts">
	// The preview (§10.10.12, §12.1): example lines for the staged events and data kinds, rendered by
	// the server from sample data (never real torrents), and the estimated volume per day for each
	// output, with how many days the files would hold. It follows the staged values through
	// `POST /settings/preview`, before anything is saved.
	import { api, isAbort, latest } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { formatBytes, formatSpan } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
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
	const DAY = 86_400;
	const locale = $derived(currentLocale());
	const size = (mib: number) => formatBytes({ value: mib * MIB, approx: true }, locale);
	const DEBOUNCE_MS = 400;

	// The staged settings of the events and outputs, plus one field the server will preview: the
	// estimates cover the whole staged section whichever field is asked about. A field set by the
	// environment (`LODESTAR_LOG` sets Verbosity) can't be sent, so the first one that isn't is.
	const GROUPS = ['events', 'console', 'file'];
	const body = $derived.by(() => {
		const staged: Record<string, unknown> = {};
		for (const s of form.specs) {
			const f = form.field(s.key);
			if (GROUPS.includes(s.group) && f.dirty && !f.locked) staged[s.name] = f.value;
		}
		if (!Object.keys(staged).length) {
			const s = form.specs.find((x) => x.preview && !form.field(x.key).locked);
			if (s) staged[s.name] = form.field(s.key).value;
		}
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
						const est =
							Object.values(p.sections.logging?.fields ?? {}).find((f) => f.estimates.length)
								?.estimates ?? [];
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

<section class="flex flex-col gap-3" aria-labelledby="logging-preview">
	<h3 id="logging-preview" class="text-body font-semibold">{m.settings_logging_preview()}</h3>
	{#if failed}
		<p class="text-small text-muted-text">{m.settings_logging_preview_failed()}</p>
	{:else if !shown}
		<p aria-busy="true" class="text-small text-muted-text">{m.settings_form_loading()}</p>
	{:else}
		<!-- Only the volumes are announced; the example lines are for reading. -->
		<ul class="flex flex-col gap-1 text-small" aria-live="polite">
			{#if shown.console !== undefined}<li data-testid="log-volume-console">
					{m.settings_logging_volume_console({ size: size(shown.console) })}
				</li>{/if}
			{#if shown.file !== undefined}<li data-testid="log-volume-file">
					{m.settings_logging_volume_file({ size: size(shown.file) })}
					{#if typeof shown.days === 'number'}{m.settings_logging_volume_days({
							span: formatSpan({ value: shown.days * DAY, approx: true }, locale)
						})}{/if}
				</li>{/if}
		</ul>
		{#if shown.examples.length}
			<p class="text-small text-muted-text">{m.settings_logging_preview_examples()}</p>
			<ul class="flex flex-col gap-2" data-testid="log-examples">
				{#each shown.examples as x, i (i)}
					<li class="flex flex-col gap-0.5 text-caption">
						{#if x.file}<span class="text-muted-text">{m.settings_logging_preview_file()}</span
							><Mono wrap>{x.file}</Mono>{/if}
						{#if x.console}<span class="text-muted-text"
								>{m.settings_logging_preview_console()}</span
							><Mono wrap>{x.console}</Mono>{/if}
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</section>
