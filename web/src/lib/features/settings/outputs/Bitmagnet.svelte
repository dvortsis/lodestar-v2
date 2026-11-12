<script lang="ts">
	// The bitmagnet export (OUT-0): what's always left out, export's own adult setting
	// (`content.export_adult`, §20.9), and the download with its progress. The browser saves the
	// file as it streams; the screen finds the export's job in `/jobs` (a download's response
	// headers aren't readable) and shows the server's counts, including a cap that cut it short:
	// the file has no trailer, so this is the only place that says so.
	import { onDestroy, onMount, type Component, type ComponentProps } from 'svelte';
	import { resolve } from '$app/paths';
	import { ApiError, api } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { EXPORT_URL, exportView, keepPolling, lastExportId, ourExport, type Job } from './logic';

	const CONTENT = { path: { section: 'content' } } as const;
	const POLL_MS = 1000;

	// export_adult: unknown until read; a lock names what sets it.
	let adult = $state(false);
	// `unreadable`: the value couldn't be read, so no switch is shown (it would claim a value).
	let adultState = $state<'unknown' | 'ready' | 'failed' | 'unreadable'>('unknown');
	let adultLock = $state<string | null>(null);
	let adultKey = $state(0);
	/** A change is being saved: the switch waits for it. */
	let saving = $state(false);
	let Dialog = $state<Component<ComponentProps<typeof import('./AdultDialog.svelte').default>>>();
	let asking = $state(false);
	let answer: ((ok: boolean) => void) | null = null;

	// The export: when it was started (Unix seconds), and its job once found.
	let clickedAt = $state<number | null>(null);
	let job = $state.raw<Job | null>(null);
	/** The highest export job id seen before the click (`lastExportId`); null until `/jobs` answers. */
	let seen: number | null = null;
	/** The first `/jobs` read, which sets `seen`; polls wait for it, so a quick click still
	 * matches by id. It never rejects. */
	let ready: Promise<void> = Promise.resolve();
	// No job was ever found (`lost`), or one was and `/jobs` stopped answering (`stale`).
	let lost = $state(false);
	let stale = $state(false);
	/** Failed `/jobs` reads in a row; a few are retried before saying the progress is lost. */
	let misses = 0;
	const MISSES = 3;
	let timer: ReturnType<typeof setTimeout> | undefined;
	/** Which press the polling belongs to: a newer one ends the older one's chain. */
	let press = 0;
	let live = true;

	const n = (v: number | null) => formatCount(v, currentLocale());
	const view = $derived(exportView(job));

	async function loadAdult() {
		try {
			const res = (await api.get('/settings/{section}', CONTENT)) as unknown as {
				fields: { export_adult?: { value?: unknown; env?: string | null; read_only?: boolean } };
			};
			const f = res.fields.export_adult;
			adult = f?.value === true;
			adultLock = f?.env ? f.env : f?.read_only ? '' : null;
			adultState = 'ready';
		} catch {
			adultState = 'unreadable';
		}
	}

	function ask(): Promise<boolean> {
		answer?.(false);
		asking = true;
		return new Promise<boolean>((done) => {
			answer = (ok) => {
				answer = null;
				asking = false;
				done(ok);
			};
		});
	}

	async function toggleAdult(on: boolean) {
		adultState = 'ready';
		try {
			if (on) {
				// The dialog (Bits UI's overlay) loads with the first turn-on, not with the screen.
				Dialog ??= (await import('./AdultDialog.svelte')).default;
				if (!(await ask())) {
					adultKey++;
					return;
				}
			}
			// `SettingsPatch` doesn't describe its fields in schema.d.ts (see flows/adult.svelte.ts).
			const body = (on
				? { export_adult: true, confirm_adult: true }
				: { export_adult: false }) as unknown as components['schemas']['SettingsPatch'];
			saving = true;
			await api.patch('/settings/{section}', { ...CONTENT, body });
			adult = on;
		} catch (e) {
			if (!(e instanceof ApiError) || e.code !== 'login_required') adultState = 'failed';
			// The switch shows what the server has.
			adultKey++;
		} finally {
			saving = false;
		}
	}

	function poll(mine: number) {
		if (!live || clickedAt === null || mine !== press) return;
		const since = clickedAt;
		// `seen` changes only once a job has finished, so it's the click's value throughout.
		const after = seen;
		api.get('/jobs').then(
			(r) => {
				if (!live || mine !== press) return;
				misses = 0;
				stale = false;
				job = ourExport(r.jobs, after, since) ?? job;
				const waited = Date.now() / 1000 - since;
				if (keepPolling(job, waited)) timer = setTimeout(() => poll(mine), POLL_MS);
				else if (!job) lost = true;
				else seen = Math.max(seen ?? 0, lastExportId(r.jobs));
			},
			() => {
				if (!live || mine !== press) return;
				if (++misses < MISSES) timer = setTimeout(() => poll(mine), POLL_MS);
				// A job already found keeps its last state; only a never-found one is lost.
				else if (job) stale = true;
				else lost = true;
			}
		);
	}

	function started() {
		// The link's own click starts the download; this only watches it.
		clearTimeout(timer);
		job = null;
		lost = false;
		stale = false;
		misses = 0;
		clickedAt = Math.floor(Date.now() / 1000);
		const mine = ++press;
		void ready.then(() => {
			if (live && mine === press) timer = setTimeout(() => poll(mine), POLL_MS / 2);
		});
	}

	onMount(() => {
		void loadAdult();
		// The ids already taken, so the next export's job is told apart without comparing clocks.
		ready = api.get('/jobs').then(
			(r) => {
				if (seen === null) seen = lastExportId(r.jobs);
			},
			() => {}
		);
	});
	onDestroy(() => {
		live = false;
		clearTimeout(timer);
		answer?.(false);
	});
</script>

<div class="flex flex-col gap-4" data-testid="outputs-bitmagnet">
	<p class="text-muted-text">{m.settings_outputs_bitmagnet_intro()}</p>
	<p data-testid="outputs-bitmagnet-left-out">{m.settings_outputs_bitmagnet_left_out()}</p>

	<div class="flex flex-col gap-2" role="group" aria-labelledby="outputs-adult">
		<h3 id="outputs-adult" class="font-semibold">{m.settings_outputs_adult_heading()}</h3>
		<p class="text-small text-muted-text">{m.settings_outputs_adult_help()}</p>
		{#if adultState === 'failed'}<Banner kind="error">{m.settings_outputs_adult_failed()}</Banner
			>{/if}
		{#if adultState === 'unreadable'}
			<p class="flex flex-wrap items-center gap-3">
				{m.settings_outputs_adult_load_failed()}
				<Button onclick={() => void loadAdult()}>{m.settings_outputs_retry()}</Button>
			</p>
		{/if}
		{#if adultLock !== null}<p class="text-small">
				{adultLock
					? m.settings_outputs_adult_locked({ variable: adultLock })
					: m.settings_outputs_adult_read_only()}
			</p>{/if}
		{#if adultState === 'ready' || adultState === 'failed'}
			{#key adultKey}
				<Toggle
					label={m.settings_outputs_adult_label()}
					checked={adult}
					disabled={adultLock !== null || saving}
					data-testid="outputs-adult"
					onchange={(e) => void toggleAdult(e.currentTarget.checked)}
				/>
			{/key}
		{/if}
		{#if Dialog}<Dialog open={asking} onanswer={(ok) => answer?.(ok)} />{/if}
	</div>

	<div class="flex flex-col gap-2">
		<div>
			<!-- eslint-disable svelte/no-navigation-without-resolve -- an API download, not a page -->
			<a
				class="btn btn-primary {FOCUS_RING} {TARGET_MIN}"
				href={EXPORT_URL}
				download
				onclick={started}
				data-testid="outputs-bitmagnet-start">{m.settings_outputs_bitmagnet_start()}</a
			>
			<!-- eslint-enable svelte/no-navigation-without-resolve -->
		</div>
		<!-- Mounted from the start, so screen readers announce what it says once an export starts.
		     The banners after it announce themselves, so they sit outside it. -->
		<div role="status" class="flex flex-col gap-1" data-testid="outputs-bitmagnet-progress">
			{#if clickedAt === null}
				<!-- nothing yet -->
			{:else if lost}
				<p>
					{m.settings_outputs_bitmagnet_lost()}
					<a class="link ls-focus" href={resolve('/settings/[section]', { section: 'jobs' })}
						>{m.settings_outputs_bitmagnet_jobs()}</a
					>
				</p>
			{:else if view.state === 'waiting'}
				<p>{m.settings_outputs_bitmagnet_waiting()}</p>
			{:else if view.state === 'running'}
				<p data-testid="outputs-bitmagnet-count">
					{view.total === null
						? m.settings_outputs_bitmagnet_running_unknown({ done: n(view.done) })
						: view.approx
							? m.settings_outputs_bitmagnet_running_approx({
									done: n(view.done),
									total: n(view.total)
								})
							: m.settings_outputs_bitmagnet_running({
									done: n(view.done),
									total: n(view.total)
								})}
				</p>
			{:else if view.state === 'paused'}
				<p data-testid="outputs-bitmagnet-count">
					{m.settings_outputs_bitmagnet_paused({ done: n(view.done) })}
				</p>
			{:else if view.state === 'done'}
				<p data-testid="outputs-bitmagnet-count">
					{m.settings_outputs_bitmagnet_done({ done: n(view.done) })}
				</p>
				{#if view.skipped > 0}<p class="text-small" data-testid="outputs-bitmagnet-skipped">
						{m.settings_outputs_bitmagnet_skipped({ skipped: n(view.skipped) })}
					</p>{/if}
			{/if}
			{#if stale}
				<p data-testid="outputs-bitmagnet-stale">
					{m.settings_outputs_bitmagnet_stale()}
					<a class="link ls-focus" href={resolve('/settings/[section]', { section: 'jobs' })}
						>{m.settings_outputs_bitmagnet_jobs()}</a
					>
				</p>
			{/if}
		</div>
		{#if clickedAt !== null && !lost}
			{#if view.state === 'done' && view.truncated}
				<Banner kind="warning"
					><span data-testid="outputs-bitmagnet-truncated"
						>{m.settings_outputs_bitmagnet_truncated({ cap: n(view.cap) })}</span
					></Banner
				>
			{:else if view.state === 'cancelled' || view.state === 'failed'}
				<Banner kind="error"
					><span data-testid="outputs-bitmagnet-count"
						>{view.state === 'cancelled'
							? m.settings_outputs_bitmagnet_cancelled({ done: n(view.done) })
							: m.settings_outputs_bitmagnet_failed({ done: n(view.done) })}</span
					></Banner
				>
			{/if}
		{/if}
	</div>

	<p class="text-small">{m.settings_outputs_bitmagnet_import()}</p>
	<!-- Focusable, so a keyboard can scroll it sideways on a narrow screen. -->
	<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
	<pre
		tabindex="0"
		aria-label={m.settings_outputs_bitmagnet_example()}
		class="overflow-x-auto rounded-box bg-base-200 p-3 font-mono text-small {FOCUS_RING}"
		data-testid="outputs-bitmagnet-curl"><code
			>curl --data-binary @lodestar-export-YYYYMMDD-HHMMSS.bitmagnet.ndjson http://localhost:3333/import</code
		></pre>
</div>
