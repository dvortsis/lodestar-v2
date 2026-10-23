<script lang="ts">
	// "Review" under the restart banner (lazy): each field waiting for a restart, its screen as a
	// link, and the value saved. The running value isn't in the API, so the note says Lodestar still
	// uses the previous one.
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { displayValue } from '../form/display';
	import { fieldLabel, loadSettingMessages } from '../form/messages';
	import { splitKey, type FieldKey, type FieldSpec } from '../form/types';
	import { useSettings } from './state.svelte';
	import type { SystemInfo } from './system';

	let { system }: { system: SystemInfo } = $props();

	const settings = useSettings();

	interface Row {
		key: FieldKey;
		label: string;
		screenId?: string;
		screen?: string;
		saved: string;
	}

	async function load(): Promise<Row[]> {
		const keys = system.restart_required as FieldKey[];
		const sections = [...new Set(keys.map((k) => splitKey(k)[0]))];
		const [values] = await Promise.all([
			Promise.all(
				sections.map(
					async (s) => [s, await api.get('/settings/{section}', { path: { section: s } })] as const
				)
			),
			loadSettingMessages(sections)
		]);
		const bySection = new Map(values);
		return keys.map((key) => {
			const [section, name] = splitKey(key);
			const spec: FieldSpec | undefined = settings.index?.spec(key);
			const home = settings.registry.screenOf(key);
			const value = bySection.get(section)?.fields[name]?.value;
			return {
				key,
				label: spec ? fieldLabel(spec) : key,
				screenId: home?.screen.id,
				screen: home?.screen.title(),
				saved: spec ? displayValue(spec, value) : String(value ?? '—')
			};
		});
	}
	const rows = $derived(load());
</script>

<section class="review" aria-labelledby="review-title" data-testid="settings-restart-review">
	<h2 id="review-title" class="text-body-lg font-semibold">{m.settings_ui_review_title()}</h2>
	{#await rows}
		<div aria-busy="true"><Skeleton lines={2} /></div>
	{:then list}
		<ul class="review-list">
			{#each list as r (r.key)}
				<li data-testid="settings-review-{r.key}">
					<!-- eslint-disable svelte/no-navigation-without-resolve -- no base path (svelte.config.js) -->
					<span class="font-medium">{r.label}</span>
					{#if r.screenId}<a href="/settings/{r.screenId}#{r.key}">{r.screen}</a>{/if}
					<!-- eslint-enable svelte/no-navigation-without-resolve -->
					<span class="text-muted-text">{m.settings_ui_review_saved({ value: r.saved })}</span>
				</li>
			{/each}
		</ul>
		<p class="text-small text-muted-text">{m.settings_ui_review_note()}</p>
	{:catch}
		<p role="alert">{m.settings_ui_review_failed()}</p>
	{/await}
</section>

<style>
	.review {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
		margin-top: calc(var(--spacing) * 2);
		padding: calc(var(--spacing) * 3);
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-box);
	}
	.review-list {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 1);
	}
	.review-list li {
		display: flex;
		flex-wrap: wrap;
		gap: calc(var(--spacing) * 1) calc(var(--spacing) * 3);
		align-items: baseline;
	}
	.review-list a {
		text-decoration: underline;
		color: var(--color-primary);
	}
</style>
