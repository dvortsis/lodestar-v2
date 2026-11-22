<script lang="ts">
	// Settings → Overview (§10.10.3): "is anything wrong, and what did I change?". Loaded on demand
	// by the route. Reads `/system`, the network check, `/workers` and every section's values; the
	// decisions are in `logic.ts`.
	import { resolve } from '$app/paths';
	import { tick } from 'svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import type { Route } from '$lib/shell/nav';
	import { api, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { statusIcons, statusInk } from '$lib/ui/status';
	import { displayValue } from '../form/display';
	import { fieldLabel, loadMessages, settingText } from '../form/messages';
	import { fieldAnchor, loadSettingsIndex } from '../form/schema';
	import { getSection } from '../form/requests';
	import type { SectionValues } from '../form/types';
	import { INERT, screens } from '../shell/screens';
	import {
		changedFromDefault,
		needsAttention,
		retryIn,
		skippedSteps,
		startHere,
		startReason,
		type Attention,
		type ChangedGroup,
		type ChangedRow,
		type StartFailure,
		type StartRow,
		type StartReason
	} from './logic';

	interface Group extends ChangedGroup {
		title: string;
	}
	let start = $state<StartRow[]>();
	let attention = $state<Attention[]>();
	let groups = $state<Group[]>();
	let attentionFailed = $state(false);
	let changedFailed = $state(false);
	let attempt = $state(0);
	let status = $state<HTMLElement>();

	// Sections no screen lists (`content`, `preferences`) belong to Appearance.
	const SCREEN_OF: Record<string, string> = { content: 'appearance', preferences: 'appearance' };
	const screenOf = (section: string) =>
		screens.find((s) => s.sections?.includes(section))?.id ??
		screens.find((s) => s.id === section)?.id ??
		SCREEN_OF[section];
	const titleOf = (id: string) => screens.find((s) => s.id === id)?.title() ?? id;

	$effect(() => {
		void attempt;
		const ctl = new AbortController();
		attention = undefined;
		groups = undefined;
		start = undefined;
		attentionFailed = changedFailed = false;
		// Health: best-effort, and shown as soon as it is in, whatever the settings reads do.
		void Promise.all([
			api.get('/system', { signal: ctl.signal }),
			api.get('/network/check', { signal: ctl.signal }).catch(() => undefined),
			api.get('/workers', { signal: ctl.signal }).catch(() => undefined)
		]).then(
			([system, network, workers]) =>
				(attention = needsAttention(system, network, screenOf, workers)),
			(e: unknown) => {
				if (!isAbort(e)) attentionFailed = true;
			}
		);
		void (async () => {
			const index = await loadSettingsIndex();
			const names = index.sections.map((s) => s.section);
			const [values] = await Promise.all([
				Promise.all(names.map((s) => getSection(s, ctl.signal))),
				loadMessages(names)
			]);
			// The skipped first-run steps are the wizard's own progress, kept in this tab.
			const skipped = skippedSteps();
			start = startHere(values as SectionValues[], skipped);
			groups = changedFromDefault(
				(key) => (INERT.has(key) ? undefined : index.spec(key)),
				values as SectionValues[],
				screenOf
			)
				.map((g) => ({ ...g, title: titleOf(g.id) }))
				.sort((a, b) => order(a.id) - order(b.id));
		})().catch((e: unknown) => {
			if (!isAbort(e)) changedFailed = true;
		});
		return () => ctl.abort();
	});

	// Navigation order, then the rest.
	const order = (id: string) => {
		const at = screens.findIndex((s) => s.id === id);
		return at < 0 ? screens.length : at;
	};
	// After Retry the button is gone: put focus on the status line that replaces it.
	function retry() {
		attempt++;
		void tick().then(() => status?.focus());
	}

	const href = (path: string) => resolve(path as Route);

	const startTitle = (r: StartRow) => {
		switch (r.id) {
			case 'client':
				return m.settings_find_start_client();
			case 'notifications':
				return m.settings_find_start_notifications();
			case 'updates':
				return m.settings_find_start_updates();
			case 'backups':
				return m.settings_find_start_backups();
			case 'password':
				return m.settings_find_start_password();
			default:
				return titleOf(r.screen);
		}
	};
	const startState = (r: StartRow) => {
		switch (r.state) {
			case 'none':
				return r.id === 'client'
					? m.settings_find_start_client_none()
					: m.settings_find_start_notifications_none();
			case 'set':
				return r.id === 'client'
					? m.settings_find_start_client_set({ count: formatCount(r.count ?? 0) })
					: r.id === 'notifications'
						? m.settings_find_start_notifications_set()
						: ((r.mode && settingText(`settings.updates.mode.option.${r.mode}`)) ?? r.mode ?? '');
			case 'off':
				return m.settings_find_start_updates_off();
			case 'skipped':
				return m.settings_find_start_skipped_state();
			default:
				return undefined;
		}
	};
	const startAction = (r: StartRow) => {
		if (r.id === 'password') return m.settings_find_start_password_action();
		if (r.configured) return m.settings_find_start_open();
		switch (r.id) {
			case 'client':
				return m.settings_find_start_client_action_none();
			case 'notifications':
				return m.settings_find_start_notifications_action_none();
			case 'updates':
				return m.settings_find_start_updates_action_off();
			case 'backups':
				return m.settings_find_start_backups_action();
			default:
				return m.settings_find_start_open();
		}
	};

	// The retry time reads "in 2 minutes", as of when the rows were built.
	const locale = $derived(currentLocale());
	const REASONS: Record<StartReason, () => string> = {
		port_in_use: m.settings_overview_workers_reason_port_in_use,
		address: m.settings_overview_workers_reason_address,
		permission: m.settings_overview_workers_reason_permission,
		port: m.settings_overview_workers_reason_port,
		database: m.settings_overview_workers_reason_database,
		secrets: m.settings_overview_workers_reason_secrets,
		certificates: m.settings_overview_workers_reason_certificates,
		server: m.settings_overview_workers_reason_server
	};
	function startFailing(f: StartFailure): string {
		const at = retryIn(f.next_retry_at, Date.now() / 1000);
		return m.settings_overview_workers_start_failing({
			reason: REASONS[startReason(f)](),
			when: at
				? new Intl.RelativeTimeFormat(locale, { numeric: 'auto' }).format(at.value, at.unit)
				: m.settings_overview_workers_soon()
		});
	}

	function sentence(a: Attention): string {
		switch (a.kind) {
			case 'rebuild_failed':
				return m.settings_overview_rebuild_failed();
			case 'address_pending':
				return m.settings_overview_address_pending();
			case 'filesystem':
				return m.settings_overview_filesystem({ detail: a.detail ?? '' });
			case 'unreachable':
				return m.settings_overview_unreachable();
			case 'restart':
				return m.settings_overview_restart({ count: formatCount(a.count ?? 0) });
			case 'errors':
				return m.settings_overview_errors({ count: formatCount(a.count ?? 0) });
			case 'workers_start_failing':
				return a.startFailure ? startFailing(a.startFailure) : '';
			case 'rebuilding':
				return m.settings_overview_rebuilding();
		}
	}

	function sourceText(r: ChangedRow): string {
		if (r.source === 'db') return m.settings_overview_source_db();
		if (r.source === 'file') return m.settings_overview_source_file();
		return r.env
			? m.settings_overview_source_env({ env: r.env })
			: m.settings_overview_source_env_unnamed();
	}
</script>

<section aria-labelledby="overview-attention" class="flex flex-col gap-3">
	<h2 id="overview-attention" class="text-body-lg font-semibold">
		{m.settings_overview_attention()}
	</h2>
	{#if attentionFailed}
		<p class="text-muted-text">{m.settings_overview_attention_unavailable()}</p>
	{:else if attention === undefined}
		<p role="status" tabindex="-1" bind:this={status} class="text-muted-text">
			{m.settings_overview_loading()}
		</p>
	{:else if attention.length === 0}
		<p data-testid="attention-empty">{m.settings_overview_attention_empty()}</p>
	{:else}
		<ul class="flex flex-col gap-2" data-testid="attention-list">
			{#each attention as a (a.kind)}
				{@const Icon = statusIcons[a.severity]}
				<li class="flex flex-wrap items-start gap-x-3 gap-y-1">
					<Icon class="mt-0.5 size-5 shrink-0 {statusInk[a.severity]}" aria-hidden="true" />
					<span class="min-w-0 flex-1" data-testid="attention-{a.kind}">{sentence(a)}</span>
					{#if a.href}
						<a class="ls-focus text-primary underline" href={resolve(a.href as Route)}
							>{m.settings_overview_go_logs()}</a
						>
					{:else if a.screen}
						<a class="ls-focus text-primary underline" href={href(`/settings/${a.screen}`)}
							>{m.settings_overview_go({ screen: titleOf(a.screen) })}</a
						>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</section>

<section aria-labelledby="overview-start" class="mt-6 flex flex-col gap-3">
	<h2 id="overview-start" class="text-body-lg font-semibold">
		{m.settings_find_start_title()}
	</h2>
	{#if start}
		<ul class="flex flex-col divide-y divide-field-border" data-testid="start-list">
			{#each start as r (r.id)}
				{@const state = startState(r)}
				<li
					class="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-2"
					data-testid="start-row"
				>
					<span class="font-medium">{startTitle(r)}</span>
					<span class="min-w-0 break-words">
						{#if state}<span class="text-muted-text">{state} · </span>{/if}<a
							class="ls-focus text-primary underline"
							href={href(`/settings/${r.screen}`)}>{startAction(r)}</a
						>
					</span>
				</li>
			{/each}
		</ul>
	{:else if !changedFailed}
		<p role="status" class="text-muted-text">{m.settings_overview_loading()}</p>
	{/if}
</section>

<section aria-labelledby="overview-changed" class="mt-6 flex flex-col gap-3">
	<h2 id="overview-changed" class="text-body-lg font-semibold">
		{m.settings_overview_changed()}
	</h2>
	{#if changedFailed}
		<div class="flex flex-wrap items-center gap-3">
			<p>{m.settings_overview_changed_unavailable()}</p>
			<Button onclick={retry}>{m.settings_overview_retry()}</Button>
		</div>
	{:else if groups === undefined}
		<p role="status" class="text-muted-text">{m.settings_overview_loading()}</p>
	{:else if groups.length === 0}
		<p data-testid="changed-empty">{m.settings_overview_changed_empty()}</p>
	{:else}
		{#each groups as g (g.id)}
			<section aria-labelledby="changed-{g.id}" class="flex flex-col gap-1">
				<h3 id="changed-{g.id}" class="font-medium">
					{#if g.linked}
						<a class="ls-focus text-primary underline" href={href(`/settings/${g.id}`)}>{g.title}</a
						>
					{:else}
						{g.title}
					{/if}
					<span class="font-normal text-muted-text"
						>· <span data-testid="changed-count"
							>{m.settings_overview_changed_count({ count: formatCount(g.rows.length) })}</span
						></span
					>
				</h3>
				<ul class="flex flex-col divide-y divide-field-border">
					{#each g.rows as r (r.spec.key)}
						<li
							class="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-2"
							data-testid="changed-row"
						>
							{#if g.linked}
								<a
									class="ls-focus text-primary underline"
									href={href(`/settings/${g.id}#${fieldAnchor(r.spec.key)}`)}
									>{fieldLabel(r.spec)}</a
								>
							{:else}
								<span>{fieldLabel(r.spec)}</span>
							{/if}
							<span class="min-w-0 break-words">
								<span class="font-mono"
									>{r.secret
										? m.settings_overview_secret_set()
										: displayValue(r.spec, r.value)}</span
								>
								<span class="text-small text-muted-text">· {sourceText(r)}</span>
							</span>
						</li>
					{/each}
				</ul>
			</section>
		{/each}
	{/if}
</section>
