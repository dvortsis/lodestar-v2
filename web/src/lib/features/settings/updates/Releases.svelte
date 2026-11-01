<script lang="ts">
	// Release notes and the history table (§10.10.14). Loaded after the first paint, so neither is
	// in the screen's first view. Notes are signed text, shown as plain text; the GitHub link opens
	// in a new tab and only when it is an https URL.
	import { m } from '$lib/paraglide/messages.js';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import { dayOf } from './time';
	import { ask } from '../form/dialogs';
	import { badges, safeHttps } from './logic';
	import type { UpdatesStore } from './store.svelte';

	let { store }: { store: UpdatesStore } = $props();
	const status = $derived(store.status);

	const badge: Record<string, () => string> = {
		fix: m.settings_updates_badge_fix,
		feature: m.settings_updates_badge_feature,
		major: m.settings_updates_badge_major,
		security: m.settings_updates_badge_security,
		manual: m.settings_updates_badge_manual
	};
	const result: Record<string, () => string> = {
		installed: m.settings_updates_result_installed,
		rolled_back: m.settings_updates_result_rolled_back,
		failed: m.settings_updates_result_failed,
		skipped: m.settings_updates_result_skipped
	};
	const day = (s: number) => dayOf(s, currentLocale());

	async function clear() {
		const answer = await ask({
			title: m.settings_updates_clear_title(),
			body: [m.settings_updates_clear_body()],
			actions: [
				{ id: 'cancel', label: m.settings_updates_cancel() },
				{ id: 'clear', label: m.settings_updates_clear_action(), variant: 'danger' }
			],
			initial: 'cancel'
		});
		if (answer === 'clear') await store.clearHistory();
	}
</script>

{#if status}
	<section
		class="flex flex-col gap-3"
		aria-labelledby="updates-releases"
		data-testid="updates-releases"
	>
		<h2 id="updates-releases" class="text-label font-semibold">
			{m.settings_updates_releases_heading()}
		</h2>
		{#each status.releases as r (r.version)}
			{@const link = safeHttps(r.notes_url)}
			<article class="flex flex-col gap-1" data-testid="updates-release-{r.version}">
				<h3 class="font-semibold">
					{m.settings_updates_release_title({ version: r.version, date: day(r.date) })}
				</h3>
				<p class="flex flex-wrap gap-1">
					{#each badges(r) as b (b)}
						<Badge variant={b === 'security' ? 'warning' : 'neutral'}>{badge[b]?.()}</Badge>
					{/each}
				</p>
				<p class="whitespace-pre-wrap">{r.notes}</p>
				{#if link}
					<p>
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- an external https URL, checked by safeHttps -->
						<a class="link" href={link} target="_blank" rel="noopener noreferrer"
							>{m.settings_updates_read_on_github()}</a
						>
					</p>
				{/if}
			</article>
		{:else}
			<p class="text-muted-text">{m.settings_updates_releases_none()}</p>
		{/each}
	</section>

	<section
		class="flex flex-col gap-3"
		aria-labelledby="updates-history"
		data-testid="updates-history"
	>
		<h2 id="updates-history" class="text-label font-semibold">
			{m.settings_updates_history_heading()}
		</h2>
		{#if status.history.length}
			<div class="overflow-x-auto">
				<table class="table w-full text-start">
					<caption class="sr-only">{m.settings_updates_history_heading()}</caption>
					<thead class="text-base-content">
						<tr>
							<th scope="col">{m.settings_updates_history_col_version()}</th>
							<th scope="col">{m.settings_updates_history_col_level()}</th>
							<th scope="col">{m.settings_updates_history_col_date()}</th>
							<th scope="col">{m.settings_updates_history_col_result()}</th>
						</tr>
					</thead>
					<tbody>
						{#each status.history as h, i (i)}
							<tr data-testid="updates-history-row">
								<th scope="row" class="font-normal">{h.version}</th>
								<td>{h.level ? (badge[h.level]?.() ?? h.level) : '—'}</td>
								<td>{day(h.at)}</td>
								<td>{result[h.result]?.() ?? h.result}{h.reason ? `: ${h.reason}` : ''}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			<div>
				<Button onclick={clear} loading={store.busy === 'clear'} data-testid="updates-history-clear"
					>{m.settings_updates_history_clear()}</Button
				>
			</div>
		{:else}
			<p class="text-muted-text">{m.settings_updates_history_none()}</p>
		{/if}
	</section>
{/if}
