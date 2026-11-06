<script lang="ts">
	// Purge a source's data (§10.10.7, §18.1): says what goes, then shows progress and the result.
	// Focus starts on Cancel. The server asks for the admin password on every purge (`428`/`401`), so
	// the dialog asks for it; it never decides a purge went through, the job's own state does.
	import { onDestroy } from 'svelte';
	import { api, ApiError, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import type { components } from '$lib/api/schema';
	import IconSpinner from '~icons/ph/circle-notch';
	import type { Source } from './sources';

	type Job = components['schemas']['BackgroundJob'];
	type Stage = components['schemas']['PurgeStage'];

	interface Props {
		open?: boolean;
		source: Source;
		/** Called when the purge has finished (done, cancelled or failed), to reload the list. */
		onfinished: () => void;
	}
	let { open = $bindable(false), source: s, onfinished }: Props = $props();

	const uid = $props.id();
	const cancelId = `${uid}-cancel`;
	const formId = `${uid}-form`;
	const POLL_MS = 1000;

	let password = $state('');
	let passwordError = $state('');
	let problem = $state('');
	let busy = $state(false);
	// Lost track of the job (the poll failed): what the user should do is said once, in `lost`.
	let lost = $state(false);
	let job = $state.raw<Job | null>(null);
	let timer: ReturnType<typeof setTimeout> | undefined;
	let ctl: AbortController | undefined;
	let destroyed = false;

	$effect(() => {
		if (open && job === null) {
			password = '';
			passwordError = '';
			problem = '';
		}
	});

	const running = $derived(
		job !== null && !lost && (job.state === 'running' || job.state === 'paused')
	);
	const stage = $derived<Stage>(job?.purge?.stage ?? 'stopping');
	const stageText = $derived(
		{
			stopping: m.sources_purge_stage_stopping,
			removing: m.sources_purge_stage_removing,
			finishing: m.sources_purge_stage_finishing,
			purged: m.sources_purge_stage_purged
		}[stage]()
	);
	const count = (n: number) => formatCount(n, currentLocale());

	async function poll(id: number) {
		ctl = new AbortController();
		try {
			const next = await api.get('/jobs/{id}', { path: { id }, signal: ctl.signal });
			if (destroyed) return;
			job = next;
			if (next.state === 'running' || next.state === 'paused') {
				timer = setTimeout(() => poll(id), POLL_MS);
			} else onfinished();
		} catch (e) {
			if (destroyed || isAbort(e)) return;
			// The job list keeps finished jobs for 10 minutes; losing track of it isn't a failure of
			// the purge itself, and the list reload shows where the source stands.
			lost = true;
			onfinished();
		}
	}

	async function submit(e: Event) {
		e.preventDefault();
		if (busy) return;
		if (password === '') {
			passwordError = m.sources_cx_password_needed();
			return;
		}
		passwordError = '';
		problem = '';
		busy = true;
		try {
			const started = await api.post('/sources/{id}/purge', {
				path: { id: s.id },
				body: { password }
			});
			password = '';
			job = started;
			if (started.state === 'running' || started.state === 'paused') void poll(started.id);
			else onfinished();
		} catch (err) {
			// The server's own sentence says what happened and what to do; it never repeats a secret.
			// (Not shared with the consent dialog: a module both import would sit in a chunk of its own,
			// which the size gate's on-demand rule doesn't know.)
			if (!(err instanceof ApiError) || err.status === 0) problem = m.sources_error_network();
			else if (err.code === 'bad_password' || err.code === 'confirm_required') {
				passwordError =
					err.code === 'bad_password'
						? m.sources_cx_wrong_password()
						: m.sources_cx_password_needed();
				password = '';
			} else if (err.code === 'purge_running') problem = m.sources_purge_busy();
			else if (err.message) problem = m.sources_cx_gate({ reason: err.message });
			else problem = m.sources_error_code({ code: err.code });
		} finally {
			busy = false;
		}
	}

	function close() {
		open = false;
	}
	$effect(() => {
		if (!open && !running) {
			job = null;
			lost = false;
			password = '';
		}
	});
	onDestroy(() => {
		destroyed = true;
		clearTimeout(timer);
		ctl?.abort();
		password = '';
		// Closed with a purge still running: the list shows where the source stands.
		if (running) onfinished();
	});
</script>

<Dialog
	bind:open
	title={m.sources_purge_title({ name: s.name })}
	closeLabel={m.sources_cx_close()}
	initialFocus={() => document.getElementById(cancelId)}
>
	<!-- Present from the start, so a change to its content is announced. -->
	<div role="status" data-testid="purge-status">
		{#if running}
			<p class="flex items-center gap-2">
				<IconSpinner class="size-5 shrink-0 motion-safe:animate-spin" aria-hidden="true" />
				{m.sources_purge_running({ name: s.name })}
			</p>
			<p class="ms-7" data-testid="purge-stage">{stageText}</p>
		{/if}
	</div>
	{#if job === null}
		<form id={formId} class="flex flex-col gap-4" onsubmit={submit} autocomplete="off" novalidate>
			<div class="flex flex-col gap-2" data-testid="purge-text">
				<p>{m.sources_purge_body({ name: s.name })}</p>
				<ul class="ms-5 flex list-disc flex-col gap-1">
					<li>{m.sources_purge_item_claims({ name: s.name })}</li>
					<li>{m.sources_purge_item_unverified()}</li>
					<li>{m.sources_purge_item_credentials()}</li>
					<li>{m.sources_purge_item_consent()}</li>
				</ul>
				<p class="font-semibold">{m.sources_purge_undo()}</p>
			</div>
			<Field
				label={m.sources_cx_password()}
				help={m.sources_cx_password_help()}
				error={passwordError}
				required
			>
				{#snippet children(c)}
					<Input
						{...c}
						type="password"
						bind:value={password}
						autocomplete="current-password"
						data-testid="purge-password"
					/>
				{/snippet}
			</Field>
			{#if problem}<div data-testid="purge-problem">
					<Banner kind="error">{problem}</Banner>
				</div>{/if}
		</form>
	{:else}
		<div class="flex flex-col gap-3" data-testid="purge-progress">
			{#if running}
				<p class="text-muted-text">{m.sources_purge_wait()}</p>
			{:else if lost}
				<Banner kind="warning">{m.sources_purge_lost()}</Banner>
			{:else if job.state === 'done'}
				<Banner kind="success">{m.sources_purge_done({ name: s.name })}</Banner>
				{#if job.purge}
					<div data-testid="purge-result">
						<p class="font-semibold">{m.sources_purge_result_heading()}</p>
						<ul class="ms-5 list-disc">
							<li data-testid="purge-torrents">
								{m.sources_purge_result_torrents({ count: count(job.purge.torrents_deleted) })}
							</li>
							<li data-testid="purge-unverified">
								{m.sources_purge_result_unverified({ count: count(job.purge.unverified_deleted) })}
							</li>
							<li data-testid="purge-kept">
								{m.sources_purge_result_kept({ count: count(job.purge.kept) })}
							</li>
						</ul>
					</div>
				{/if}
			{:else if job.state === 'cancelled'}
				<Banner kind="warning">{m.sources_purge_cancelled({ name: s.name })}</Banner>
			{:else}
				<Banner kind="error">{m.sources_purge_failed({ name: s.name })}</Banner>
			{/if}
		</div>
	{/if}
	{#snippet footer()}
		{#if job === null}
			<Button id={cancelId} onclick={close} data-testid="purge-cancel"
				>{m.sources_cx_cancel()}</Button
			>
			<Button
				variant="danger"
				type="submit"
				form={formId}
				loading={busy}
				data-testid="purge-confirm"
			>
				{m.sources_purge_confirm({ name: s.name })}
			</Button>
		{:else}
			<Button id={cancelId} onclick={close} data-testid="purge-close">{m.sources_cx_close()}</Button
			>
		{/if}
	{/snippet}
</Dialog>
