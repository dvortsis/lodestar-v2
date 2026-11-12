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
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { EXPORT_URL, exportView, keepPolling, ourExport, type Job } from './logic';

	const CONTENT = { path: { section: 'content' } } as const;
	const POLL_MS = 1000;

	// export_adult: unknown until read; a lock names what sets it.
	let adult = $state(false);
	let adultState = $state<'unknown' | 'ready' | 'failed'>('unknown');
	let adultLock = $state<string | null>(null);
	let adultKey = $state(0);
	let Dialog = $state<Component<ComponentProps<typeof import('./AdultDialog.svelte').default>>>();
	let asking = $state(false);
	let answer: ((ok: boolean) => void) | null = null;

	// The export: when it was started (Unix seconds), and its job once found.
	let clickedAt = $state<number | null>(null);
	let job = $state.raw<Job | null>(null);
	let lost = $state(false);
	let timer: ReturnType<typeof setTimeout> | undefined;
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
			adultLock = f?.env ? f.env : null;
			adultState = 'ready';
		} catch {
			adultState = 'failed';
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
			await api.patch('/settings/{section}', { ...CONTENT, body });
			adult = on;
		} catch (e) {
			if (!(e instanceof ApiError) || e.code !== 'login_required') adultState = 'failed';
			// The switch shows what the server has.
			adultKey++;
		}
	}

	function poll() {
		if (!live || clickedAt === null) return;
		const since = clickedAt;
		api.get('/jobs').then(
			(r) => {
				if (!live || clickedAt !== since) return;
				job = ourExport(r.jobs, since) ?? job;
				const waited = Date.now() / 1000 - since;
				if (keepPolling(job, waited)) timer = setTimeout(poll, POLL_MS);
				else if (!job) lost = true;
			},
			() => {
				if (!live || clickedAt !== since) return;
				lost = true;
			}
		);
	}

	function started() {
		// The link's own click starts the download; this only watches it.
		clearTimeout(timer);
		job = null;
		lost = false;
		clickedAt = Math.floor(Date.now() / 1000);
		timer = setTimeout(poll, POLL_MS / 2);
	}

	onMount(() => void loadAdult());
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
		{#if adultLock}<p class="text-small">
				{m.settings_outputs_adult_locked({ variable: adultLock })}
			</p>{/if}
		{#if adultState !== 'unknown'}
			{#key adultKey}
				<Toggle
					label={m.settings_outputs_adult_label()}
					checked={adult}
					disabled={!!adultLock}
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
		{#if clickedAt !== null}
			<div role="status" class="flex flex-col gap-1" data-testid="outputs-bitmagnet-progress">
				{#if lost}
					<p>
						{m.settings_outputs_bitmagnet_lost()}
						<a class="link" href={resolve('/settings/[section]', { section: 'jobs' })}
							>{m.settings_outputs_bitmagnet_jobs()}</a
						>
					</p>
				{:else if view.state === 'waiting'}
					<p aria-busy="true">{m.settings_outputs_bitmagnet_waiting()}</p>
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
					{#if view.truncated}<Banner kind="warning"
							><span data-testid="outputs-bitmagnet-truncated"
								>{m.settings_outputs_bitmagnet_truncated({ cap: n(view.cap) })}</span
							></Banner
						>{/if}
				{:else}
					<Banner kind="error"
						><span data-testid="outputs-bitmagnet-count"
							>{view.state === 'cancelled'
								? m.settings_outputs_bitmagnet_cancelled({ done: n(view.done) })
								: m.settings_outputs_bitmagnet_failed({ done: n(view.done) })}</span
						></Banner
					>
				{/if}
			</div>
		{/if}
	</div>

	<p class="text-small">{m.settings_outputs_bitmagnet_import()}</p>
	<pre class="overflow-x-auto rounded-box bg-base-200 p-3 font-mono text-small"><code
			>curl --data-binary @lodestar-export.bitmagnet.ndjson http://bitmagnet:3333/import</code
		></pre>
</div>
