<script lang="ts">
	// Review: each pending field with its saved value (§10.10.6). The API sends no running value
	// (decisions file), so the list says what is saved and that it applies after the restart. Loaded
	// when Review is first opened, with the settings form engine it reads labels and values through.
	import { m } from '$lib/paraglide/messages.js';
	import { displayValue } from '../settings/form/display';
	import { fieldLabel, loadMessages } from '../settings/form/messages';
	import { getSection } from '../settings/form/requests';
	import { loadSettingsIndex } from '../settings/form/schema';

	let { fields }: { fields: string[] } = $props();
	let rows = $state<{ key: string; label: string; value: string }[]>();
	let failed = $state(false);

	$effect(() => {
		const keys = fields;
		let live = true;
		void (async () => {
			const sections = [...new Set(keys.map((f) => f.split('.')[0]!))];
			const [index, values] = await Promise.all([
				loadSettingsIndex(),
				Promise.all(sections.map((s) => getSection(s))),
				loadMessages(sections)
			]).then(([i, v]) => [i, v] as const);
			const next = keys.map((key) => {
				const [section, name] = key.split('.') as [string, string];
				const spec = index.spec(key as never);
				const v = values.find((s) => s.section === section)?.fields[name];
				return {
					key,
					label: spec ? fieldLabel(spec) : key,
					value:
						!spec || !v
							? '—'
							: spec.writeOnly
								? m.restart_secret_set()
								: displayValue(spec, v.value)
				};
			});
			if (live) rows = next;
		})().catch(() => {
			if (live) failed = true;
		});
		return () => (live = false);
	});
</script>

<section aria-label={m.restart_review_title()} data-testid="restart-review">
	{#if failed}
		<p>{m.restart_review_failed()}</p>
	{:else if !rows}
		<p role="status">{m.restart_review_loading()}</p>
	{:else}
		<ul class="flex flex-col divide-y divide-field-border">
			{#each rows as r (r.key)}
				<li class="flex flex-wrap justify-between gap-x-4 py-2" data-testid="restart-row">
					<span>{r.label}</span>
					<span>
						<span class="font-mono">{m.restart_saved()}: {r.value}</span>
						<span class="text-small text-muted-text">· {m.restart_applies()}</span>
					</span>
				</li>
			{/each}
		</ul>
	{/if}
</section>
