<script lang="ts">
	// Settings → Jobs (SRC-16, §18.8, §9): the background jobs of this server with progress, rate
	// and ETA, and pause, resume and cancel. Every state shown is the server's: a control sends its
	// request and the job's row becomes what the server answers, never a guess made first.
	import { onMount, tick } from 'svelte';
	import { ApiError, api, isAbort } from '$lib/api/client';
	import { formatCount, formatDateTime, formatPercent, formatSpan } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Badge, { type BadgeVariant } from '$lib/ui/labels/Badge.svelte';
	import {
		anyActive,
		controls,
		processed,
		ratio,
		viewOf,
		type Job,
		type JobControl,
		type JobView
	} from './jobs';

	/** How often the list refreshes while a job is moving, and while none is. */
	const BUSY_MS = 2000;
	const IDLE_MS = 10_000;

	let status = $state<'loading' | 'ready' | 'failed'>('loading');
	let jobs = $state.raw<Job[]>([]);
	/** The last refresh failed: the rows may be out of date. */
	let stale = $state(false);
	/** Jobs with a control request in flight, and the reason a request was refused. */
	let busy = $state<Record<number, JobControl | undefined>>({});
	let refused = $state<Record<number, string | undefined>>({});
	let announcement = $state('');
	/** Bumped by every control answer, so a list fetched before it can't overwrite it. */
	let epoch = 0;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let ctl: AbortController | undefined;

	const locale = () => currentLocale();
	const kindName = (j: Job) => (j.kind === 'import' ? m.jobs_kind_import() : m.jobs_kind_export());
	const stateName = (v: JobView) =>
		({
			running: m.jobs_state_running,
			paused: m.jobs_state_paused,
			cancelling: m.jobs_state_cancelling,
			cancelled: m.jobs_state_cancelled,
			done: m.jobs_state_done,
			failed: m.jobs_state_failed
		})[v]();
	const variant: Record<JobView, BadgeVariant> = {
		running: 'info',
		paused: 'warning',
		cancelling: 'warning',
		cancelled: 'neutral',
		done: 'success',
		failed: 'error'
	};
	const count = (n: number) => formatCount(n, locale());
	const rate = (r: number) =>
		m.jobs_rate({
			n: new Intl.NumberFormat(locale(), { maximumFractionDigits: r < 10 ? 1 : 0 }).format(r)
		});
	const progressText = (j: Job) => {
		const read = count(processed(j));
		if (j.total === null) return m.jobs_progress_unknown({ read });
		const args = { read, total: count(j.total) };
		return j.total_approx ? m.jobs_progress_approx(args) : m.jobs_progress_exact(args);
	};
	const controlText = (c: JobControl) =>
		({ pause: m.jobs_pause, resume: m.jobs_resume, cancel: m.jobs_cancel })[c]();
	const controlLabel = (c: JobControl, j: Job) =>
		({ pause: m.jobs_pause_label, resume: m.jobs_resume_label, cancel: m.jobs_cancel_label })[c]({
			kind: kindName(j),
			id: j.id
		});

	function schedule(ms: number) {
		clearTimeout(timer);
		timer = setTimeout(() => void refresh(), ms);
	}

	async function refresh() {
		ctl?.abort();
		const mine = (ctl = new AbortController());
		const startedAt = epoch;
		try {
			const res = await api.get('/jobs', { signal: mine.signal });
			if (mine.signal.aborted || startedAt !== epoch) return;
			jobs = res.jobs;
			stale = false;
			status = 'ready';
		} catch (e) {
			if (isAbort(e)) return;
			// A 401 is the shell's. Rows already shown stay, marked as possibly out of date.
			if (status === 'ready') stale = true;
			else status = 'failed';
		}
		if (!mine.signal.aborted && status !== 'failed') schedule(anyActive(jobs) ? BUSY_MS : IDLE_MS);
	}

	async function control(j: Job, c: JobControl) {
		if (busy[j.id]) return;
		busy[j.id] = c;
		refused[j.id] = undefined;
		try {
			const path = `/jobs/{id}/${c}` as const;
			const next = await api.post(path, { path: { id: j.id }, body: {} });
			epoch++;
			jobs = jobs.map((x) => (x.id === next.id ? next : x));
			announcement = m.jobs_announce({
				kind: kindName(next),
				id: next.id,
				state: stateName(viewOf(next))
			});
			// The pressed button is replaced (Pause by Resume) or gone: keep focus in the row.
			await tick();
			const row = document.getElementById(`job-${next.id}`);
			(
				row?.querySelector<HTMLElement>('[data-control]') ?? row?.querySelector<HTMLElement>('h2')
			)?.focus();
		} catch (e) {
			if (isAbort(e)) return;
			refused[j.id] = m.jobs_action_failed({
				reason: e instanceof ApiError ? e.message : String(e)
			});
			// The job may have finished or expired meanwhile: show what the server has now.
			void refresh();
		} finally {
			busy[j.id] = undefined;
		}
	}

	function retry() {
		status = 'loading';
		void refresh();
	}

	onMount(() => {
		void refresh();
		return () => {
			clearTimeout(timer);
			ctl?.abort();
		};
	});
</script>

<h1 id="jobs-title" tabindex="-1" class="text-title font-semibold">{m.jobs_title()}</h1>
<p class="text-muted-text">{m.jobs_purpose()}</p>

<p class="sr-only" role="status" data-testid="jobs-announce">{announcement}</p>

{#if status === 'failed'}
	<Banner kind="error">
		{m.jobs_load_error()}
		{#snippet action()}<Button dense onclick={retry} data-testid="jobs-retry"
				>{m.jobs_retry()}</Button
			>{/snippet}
	</Banner>
{:else if status === 'loading'}
	<p role="status" aria-busy="true" data-testid="jobs-loading">{m.jobs_loading()}</p>
{:else}
	{#if stale}
		<Banner kind="warning"><span data-testid="jobs-stale">{m.jobs_refresh_error()}</span></Banner>
	{/if}
	{#if jobs.length === 0}
		<EmptyState
			headingLevel={2}
			title={m.jobs_empty_title()}
			text={m.jobs_empty_text()}
			data-testid="jobs-empty"
		/>
	{:else}
		<ul class="flex flex-col gap-4" aria-label={m.jobs_list_label()} data-testid="jobs-list">
			{#each jobs as j (j.id)}
				{@const v = viewOf(j)}
				{@const share = ratio(j)}
				{@const title = m.jobs_item_title({ kind: kindName(j), id: j.id })}
				<li
					class="flex flex-col gap-2 rounded-box border border-field-border p-4"
					id="job-{j.id}"
					data-testid="job-{j.id}"
					data-state={v}
				>
					<div class="flex flex-wrap items-center justify-between gap-2">
						<h2 tabindex="-1" class="text-body font-semibold">{title}</h2>
						<Badge variant={variant[v]}
							><span data-testid="job-{j.id}-state">{stateName(v)}</span></Badge
						>
					</div>

					<div class="flex flex-col gap-1">
						<div class="flex flex-wrap justify-between gap-2 text-small">
							<span>{m.jobs_field_progress()}</span>
							<span data-testid="job-{j.id}-progress">
								{progressText(j)}{#if share !== null}
									({formatPercent(share, locale(), 0)}){/if}
							</span>
						</div>
						<progress
							class="progress w-full"
							value={share === null ? undefined : share}
							max={share === null ? undefined : 1}
							aria-label={m.jobs_progress_label({ kind: kindName(j), id: j.id })}
						></progress>
					</div>

					<dl class="grid grid-cols-2 gap-x-4 gap-y-1 text-small">
						<dt class="text-muted-text">{m.jobs_field_done()}</dt>
						<dd>{count(j.done)}</dd>
						<dt class="text-muted-text">{m.jobs_field_skipped()}</dt>
						<dd>{count(j.skipped)}</dd>
						{#if v === 'running'}
							{#if j.rate !== null}
								<dt class="text-muted-text">{m.jobs_field_rate()}</dt>
								<dd data-testid="job-{j.id}-rate">{rate(j.rate)}</dd>
							{/if}
							{#if j.eta_s !== null}
								<dt class="text-muted-text">{m.jobs_field_eta()}</dt>
								<dd data-testid="job-{j.id}-eta">{formatSpan(j.eta_s, locale())}</dd>
							{/if}
						{/if}
						<dt class="text-muted-text">{m.jobs_field_started()}</dt>
						<dd>{formatDateTime(j.started_at, locale())}</dd>
						{#if j.finished_at !== null}
							<dt class="text-muted-text">{m.jobs_field_finished()}</dt>
							<dd>{formatDateTime(j.finished_at, locale())}</dd>
						{/if}
					</dl>

					{#if j.export}
						<p class="text-small text-muted-text">
							{m.jobs_export_details({
								format: j.export.format.toUpperCase(),
								cap: count(j.export.cap)
							})}
						</p>
						{#if j.export.truncated}
							<p class="text-small" data-testid="job-{j.id}-truncated">
								{m.jobs_export_truncated({ cap: count(j.export.cap) })}
							</p>
						{/if}
					{/if}
					{#if v === 'paused'}
						<p class="text-small text-muted-text">{m.jobs_paused_note()}</p>
					{:else if v === 'cancelling'}
						<p class="text-small text-muted-text">{m.jobs_cancelling_note()}</p>
					{/if}
					{#if j.error}
						<p class="text-small text-error" data-testid="job-{j.id}-error">{j.error.message}</p>
					{/if}
					{#if refused[j.id]}
						<p role="alert" class="text-small text-error" data-testid="job-{j.id}-refused">
							{refused[j.id]}
						</p>
					{/if}

					{#if controls(j).length}
						<div class="flex flex-wrap gap-2">
							{#each controls(j) as c (c)}
								<Button
									dense
									variant={c === 'cancel' ? 'danger' : 'secondary'}
									loading={busy[j.id] === c}
									disabled={!!busy[j.id] && busy[j.id] !== c}
									aria-label={controlLabel(c, j)}
									data-testid="job-{j.id}-{c}"
									data-control
									onclick={() => control(j, c)}>{controlText(c)}</Button
								>
							{/each}
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
{/if}
