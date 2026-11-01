<script lang="ts">
	// Settings → About (§10.10.13): what is running, whether there is a newer version, where to get
	// help and what Lodestar connects to. One GET /system feeds nearly all of it; the update summary
	// reads GET /updates (the full screen is Settings → Updates). The diagnostics and the licence
	// list are their own parts, loaded when opened.
	import { onMount, type Component } from 'svelte';
	import { resolve } from '$app/paths';
	import { api, isAbort } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { logos } from '$lib/brand/scenes.generated';
	import { formatBytes, formatCount, formatDateTime } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { copyText } from '$lib/util/clipboard';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { screens } from '../shell/screens';
	import {
		httpsUrl,
		isStale,
		links,
		shortCommit,
		UI_BUILD as uiBuild,
		versionText,
		type System
	} from './logic';

	type Updates = components['schemas']['UpdatesStatus'];

	const locale = $derived(currentLocale());
	let info = $state.raw<System>();
	let failed = $state(false);
	let attempt = $state(0);
	let updates = $state.raw<Updates>();
	let updatesFailed = $state(false);
	let checking = $state(false);
	let checkFailed = $state(false);
	let copied = $state<'copied' | 'manual'>();
	let Diagnostics = $state.raw<Component<{ info: System; uiBuild: string }>>();
	let Licences = $state.raw<Component>();
	let partFailed = $state<'diagnostics' | 'licences'>();

	$effect(() => {
		void attempt;
		let live = true;
		failed = false;
		api.get('/system').then(
			(s) => live && (info = s),
			(e) => live && !isAbort(e) && (failed = true)
		);
		api.get('/updates').then(
			(u) => live && ((updates = u), (updatesFailed = false)),
			(e) => live && !isAbort(e) && (updatesFailed = true)
		);
		return () => (live = false);
	});

	// The lockup follows the theme's mode (set on <html> by the theme switcher).
	let dark = $state(false);
	onMount(() => (dark = document.documentElement.style.colorScheme === 'dark'));
	const logo = $derived(
		logos.find((l) => l.id === 'horizontal' && l.mode === (dark ? 'dark' : 'light'))!
	);

	// A link to a Settings screen only while that screen exists (Updates is a later slice).
	const known = new Set(screens.map((s) => s.id));
	const stale = $derived(isStale(uiBuild, info?.build.ui_build_id));
	const link = $derived(info ? links(info) : null);
	const fmtBytes = (n: number | null) => formatBytes(n, locale);
	const channel = $derived(
		info
			? {
					stable: m.settings_about_channel_stable,
					beta: m.settings_about_channel_beta,
					nightly: m.settings_about_channel_nightly,
					dev: m.settings_about_channel_dev
				}[info.channel]()
			: ''
	);
	const deployment = $derived(
		info
			? (
					{
						docker: m.settings_about_deployment_docker,
						unraid: m.settings_about_deployment_unraid,
						windows_service: m.settings_about_deployment_windows_service,
						windows_portable: m.settings_about_deployment_windows_portable,
						systemd: m.settings_about_deployment_systemd,
						tarball: m.settings_about_deployment_tarball,
						unknown: m.settings_about_deployment_unknown
					} as const
				)[info.deployment.kind]()
			: ''
	);
	const connLabels = {
		dht: m.settings_about_conn_dht,
		sources: m.settings_about_conn_sources,
		blocklist_subscriptions: m.settings_about_conn_blocklist_subscriptions,
		integrations: m.settings_about_conn_integrations,
		updates: m.settings_about_conn_updates,
		companions: m.settings_about_conn_companions,
		workers: m.settings_about_conn_workers
	};
	const stateLabel = (s: 'on' | 'paused' | 'off' | null) =>
		s === null
			? m.settings_about_state_none()
			: {
					on: m.settings_about_state_on,
					paused: m.settings_about_state_paused,
					off: m.settings_about_state_off
				}[s]();

	async function copyVersion() {
		if (info) copied = await copyText(versionText(info, uiBuild));
	}
	async function check() {
		checking = true;
		checkFailed = false;
		try {
			updates = await api.post('/updates/check');
			updatesFailed = false;
		} catch (e) {
			if (!isAbort(e)) checkFailed = true;
		} finally {
			checking = false;
		}
	}
	async function openDiagnostics() {
		partFailed = undefined;
		try {
			Diagnostics = (await import('./Diagnostics.svelte')).default;
		} catch {
			partFailed = 'diagnostics';
		}
	}
	async function openLicences() {
		partFailed = undefined;
		try {
			Licences = (await import('./Licences.svelte')).default;
		} catch {
			partFailed = 'licences';
		}
	}
	const newest = $derived(updates?.releases[0]);
</script>

{#snippet ext(href: string, label: string)}
	<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- an external https link to the build's repository (logic.repoBase) -->
	<a class="underline" {href} target="_blank" rel="noopener noreferrer"
		>{label}<span class="sr-only"> {m.settings_about_new_tab()}</span></a
	>
{/snippet}
{#snippet row(label: string, value: string | number | null | undefined)}
	{#if value !== null && value !== undefined && value !== ''}
		<div class="flex flex-wrap gap-2">
			<dt class="text-muted-text">{label}</dt>
			<dd class="min-w-0 break-words tabular-nums">{value}</dd>
		</div>
	{/if}
{/snippet}

<h1 id="about-title" tabindex="-1" class="sr-only">{m.settings_about_title()}</h1>
<header class="flex flex-col gap-2">
	<picture>
		<source type="image/avif" srcset={logo.src.avif} />
		<img
			src={logo.src.webp.split(' ')[0]}
			srcset={logo.src.webp}
			width={logo.width}
			height={logo.height}
			alt="Lodestar"
			class="h-auto max-w-full"
			style="width: {logo.width}px"
		/>
	</picture>
	<p class="text-body-lg">{m.settings_about_tagline()}</p>
	<p class="text-muted-text">{m.settings_about_what()}</p>
</header>

{#if stale}
	<Banner kind="warning"
		>{m.settings_about_stale()}{#snippet action()}<Button dense onclick={() => location.reload()}
				>{m.settings_about_reload()}</Button
			>{/snippet}</Banner
	>
{/if}

{#if failed}
	<Banner kind="error"
		>{m.settings_about_load_error({ id: uiBuild })}{#snippet action()}<Button
				dense
				onclick={() => attempt++}>{m.settings_about_retry()}</Button
			>{/snippet}</Banner
	>
{:else if !info}
	<p aria-busy="true" role="status">{m.settings_about_loading()}</p>
{:else}
	{@const b = info.build}
	{#if info.fixture}<Banner kind="info">{m.settings_about_fixture()}</Banner>{/if}

	<section aria-labelledby="about-version" class="flex flex-col gap-3">
		<h2 id="about-version" class="text-display font-semibold">
			{m.settings_about_version({ version: info.version })}
		</h2>
		<p class="text-muted-text">
			{channel}{#if b.date !== null}
				{' '}· {m.settings_about_released({ date: formatDateTime(b.date, locale) })}{/if}
		</p>
		<dl class="flex flex-col gap-1">
			{#if b.commit}
				<div class="flex flex-wrap gap-2">
					<dt class="text-muted-text">{m.settings_about_commit()}</dt>
					<dd class="min-w-0">
						<Mono>
							{#if link?.commit}{@render ext(
									link.commit,
									shortCommit(b.commit) ?? ''
								)}{:else}{shortCommit(b.commit)}{/if}
						</Mono>
					</dd>
				</div>
			{/if}
			{@render row(
				m.settings_about_build_date(),
				b.date === null ? null : formatDateTime(b.date, locale)
			)}
			{@render row(m.settings_about_target(), b.target)}
			{@render row(m.settings_about_rust(), b.rustc)}
			{@render row(m.settings_about_ui_build(), b.ui_build_id ?? uiBuild)}
		</dl>
		<div class="flex flex-wrap items-center gap-3">
			<Button onclick={copyVersion}>{m.settings_about_copy_version()}</Button>
			{#if link}
				{@render ext(link.releases, m.settings_about_notes_all())}
			{/if}
			<span role="status" class="text-small text-muted-text"
				>{copied === 'copied'
					? m.settings_about_copied()
					: copied
						? m.settings_about_copy_manual()
						: ''}</span
			>
		</div>
	</section>

	<section aria-labelledby="about-formats" class="flex flex-col gap-2">
		<h2 id="about-formats" class="text-body-lg font-semibold">{m.settings_about_formats()}</h2>
		<dl class="flex flex-col gap-1">
			{@render row(m.settings_about_store_schema(), info.data_formats.store_schema)}
			{@render row(m.settings_about_index(), info.data_formats.index)}
			{@render row(
				m.settings_about_rebuild(),
				info.data_formats.rebuild
					? info.data_formats.rebuild.phase === 'failed'
						? m.settings_about_rebuild_failed()
						: info.data_formats.rebuild.phase === 'done'
							? m.settings_about_rebuild_idle()
							: m.settings_about_rebuild_running({
									done: formatCount(info.data_formats.rebuild.done, locale),
									total: formatCount(info.data_formats.rebuild.total, locale)
								})
					: m.settings_about_rebuild_idle()
			)}
		</dl>
	</section>

	<section aria-labelledby="about-install" class="flex flex-col gap-2">
		<h2 id="about-install" class="text-body-lg font-semibold">{m.settings_about_install()}</h2>
		<dl class="flex flex-col gap-1">
			{@render row(m.settings_about_deployment(), deployment)}
			{@render row(
				m.settings_about_image(),
				[info.deployment.image_tag, info.deployment.image_digest].filter(Boolean).join(' @ ')
			)}
			{@render row(m.settings_about_os(), `${info.host.os} ${info.host.arch}`)}
			{@render row(
				m.settings_about_cpus(),
				info.host.cpus === null ? null : formatCount(info.host.cpus, locale)
			)}
			{@render row(
				m.settings_about_memory(),
				info.host.memory_bytes === null ? null : fmtBytes(info.host.memory_bytes)
			)}
			{@render row(m.settings_about_time_zone(), info.host.time_zone)}
			<div class="flex flex-wrap gap-2">
				<dt class="text-muted-text">{m.settings_about_started()}</dt>
				<dd>
					<RelativeTime seconds={info.started_at} timeZone={info.host.time_zone ?? undefined} />
				</dd>
			</div>
			{@render row(m.settings_about_data_dir(), info.data.dir)}
			{@render row(
				m.settings_about_data_db(),
				info.data.database_bytes === null ? null : fmtBytes(info.data.database_bytes)
			)}
			{@render row(
				m.settings_about_data_index(),
				info.data.index_bytes === null ? null : fmtBytes(info.data.index_bytes)
			)}
			{@render row(
				m.settings_about_data_cold(),
				info.data.cold_bytes === null ? null : fmtBytes(info.data.cold_bytes)
			)}
			{@render row(
				m.settings_about_data_logs(),
				info.data.logs_bytes === null ? null : fmtBytes(info.data.logs_bytes)
			)}
			{@render row(
				m.settings_about_data_free(),
				info.data.free_bytes === null ? null : fmtBytes(info.data.free_bytes)
			)}
		</dl>
		{#if info.data.filesystem_warning}<Banner kind="warning">{info.data.filesystem_warning}</Banner
			>{/if}
	</section>

	<section aria-labelledby="about-updates" class="flex flex-col gap-2">
		<h2 id="about-updates" class="text-body-lg font-semibold">{m.settings_about_updates()}</h2>
		{#if updatesFailed}
			<p role="alert" class="text-error">{m.settings_about_updates_load_error()}</p>
		{:else if !updates}
			<p aria-busy="true" role="status">{m.settings_about_loading()}</p>
		{:else if updates.unavailable}
			<p>{m.settings_about_updates_unavailable()}</p>
		{:else}
			<p role="status">
				{#if updates.status.error}
					<span data-testid="about-check-error"
						>{m.settings_about_updates_error({ code: updates.status.error })}</span
					>
				{:else if newest}
					{m.settings_about_updates_available({ version: newest.version })}
					{#if httpsUrl(newest.notes_url)}{@render ext(
							httpsUrl(newest.notes_url)!,
							m.settings_about_notes()
						)}{/if}
				{:else if updates.status.checked_at}
					{m.settings_about_updates_latest()}
					<RelativeTime seconds={updates.status.checked_at} />
				{:else}
					{m.settings_about_updates_never()}
				{/if}
			</p>
		{/if}
		{#if checkFailed}<p role="alert" class="text-error">{m.settings_about_check_failed()}</p>{/if}
		<div class="flex flex-wrap items-center gap-3">
			{#if updates && !updates.unavailable}
				<Button loading={checking} onclick={check}
					>{checking ? m.settings_about_checking() : m.settings_about_check_now()}</Button
				>
			{/if}
			{#if known.has('updates')}
				<a class="underline" href={resolve('/settings/[section]', { section: 'updates' })}
					>{m.settings_about_updates_settings()}</a
				>{/if}
		</div>
	</section>

	<section aria-labelledby="about-help" class="flex flex-col gap-2">
		<h2 id="about-help" class="text-body-lg font-semibold">{m.settings_about_help()}</h2>
		{#if link}
			<ul class="flex flex-col gap-1">
				<li>{@render ext(link.repo, m.settings_about_link_repo())}</li>
				<li>{@render ext(link.source, m.settings_about_link_source())}</li>
				<li>{@render ext(link.docs, m.settings_about_link_docs())}</li>
				<li>{@render ext(link.releases, m.settings_about_link_releases())}</li>
				<li>{@render ext(link.feature, m.settings_about_link_feature())}</li>
				<li>{@render ext(link.discussions, m.settings_about_link_discussions())}</li>
				<li>{@render ext(link.security, m.settings_about_link_security())}</li>
			</ul>
		{:else}
			<p class="text-muted-text">{m.settings_about_no_repo()}</p>
		{/if}
	</section>

	<section aria-labelledby="about-privacy" class="flex flex-col gap-2">
		<h2 id="about-privacy" class="text-body-lg font-semibold">{m.settings_about_privacy()}</h2>
		<p class="text-muted-text">{m.settings_about_privacy_lead()}</p>
		<ul class="flex flex-col gap-1">
			{#each info.connections as c (c.kind)}
				<li>
					{#if known.has(c.settings)}<a
							class="underline"
							href={resolve('/settings/[section]', { section: c.settings })}
							>{connLabels[c.kind]()}</a
						>{:else}{connLabels[c.kind]()}{/if}: {stateLabel(c.state)}{#if c.count !== null}
						· {m.settings_about_conn_count({
							count: formatCount(c.count, locale)
						})}{/if}{#if c.last_checked_at !== null}
						· {m.settings_about_conn_checked()}<RelativeTime seconds={c.last_checked_at} />{/if}
				</li>
			{/each}
		</ul>
	</section>

	<section aria-labelledby="about-diagnostics" class="flex flex-col gap-2">
		<h2 id="about-diagnostics" class="text-body-lg font-semibold">
			{m.settings_about_diagnostics()}
		</h2>
		<p class="text-muted-text">{m.settings_about_diagnostics_lead()}</p>
		{#if Diagnostics}
			<Diagnostics {info} {uiBuild} />
		{:else}
			<div><Button onclick={openDiagnostics}>{m.settings_about_diagnostics_show()}</Button></div>
		{/if}
	</section>

	<section aria-labelledby="about-licences" class="flex flex-col gap-2">
		<h2 id="about-licences" class="text-body-lg font-semibold">{m.settings_about_licences()}</h2>
		<p>{m.settings_about_licence_own()}</p>
		<ul class="text-muted-text">
			<li>{m.settings_about_credit_fonts()}</li>
			<li>{m.settings_about_credit_icons()}</li>
			<li>{m.settings_about_credit_brand()}</li>
		</ul>
		{#if Licences}
			<Licences />
		{:else}
			<div><Button onclick={openLicences}>{m.settings_about_licence_load()}</Button></div>
		{/if}
	</section>
{/if}
{#if partFailed}
	<p role="alert" class="text-small text-error">
		{partFailed === 'licences'
			? m.settings_about_licence_error()
			: m.settings_about_diagnostics_error()}
	</p>
{/if}
