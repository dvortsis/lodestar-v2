<script lang="ts">
	// Log files (§10.10.12, §12.1): the list with name, size, time range and a "Contains torrent
	// details" badge; Download per file and Download all (zip), as text or JSON lines, with
	// "Remove torrent details" on by default; the folder with Copy; on Docker, where the console log
	// is. Then the Danger zone: Delete log files (all, or only those with torrent details), each
	// behind a dialog with the count and size, focus on Cancel.
	import IconWarn from '~icons/ph/warning';
	import { api, isAbort } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { formatBytes, formatCount, formatDateTime } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { copyText } from '$lib/util/clipboard';
	import { ask } from '../form/dialogs';
	import type { LoggingCatalog } from './catalog.svelte';

	type Files = components['schemas']['LogFiles'];
	type File = components['schemas']['LogFileInfo'];

	let { catalog }: { catalog: LoggingCatalog } = $props();

	let list = $state.raw<Files>();
	let failed = $state(false);
	async function load() {
		try {
			list = await api.get('/logs/files');
			failed = false;
		} catch (e) {
			if (!isAbort(e)) failed = true;
		}
	}
	void load();

	let format = $state<'text' | 'jsonl'>('text');
	// On by default (§12.1); it matters only for files that have torrent details.
	let redact = $state(true);
	const files = $derived(list?.files ?? []);
	const anyDetails = $derived(files.some((f) => f.details || f.opt_in));
	const query = (hasDetails: boolean) =>
		`?format=${format}${hasDetails ? `&redact=${redact}` : ''}`;
	const fileHref = (f: File) =>
		`/api/v1/logs/files/${encodeURIComponent(f.name)}${query(f.details || f.opt_in)}`;
	const archiveHref = $derived(`/api/v1/logs/archive.zip${query(anyDetails)}`);
	const when = (s: string | null) => (s ? formatDateTime(Date.parse(s) / 1000) : '—');

	let copied = $state<'copied' | 'manual' | null>(null);
	let folderEl = $state<HTMLElement>();
	async function copyFolder() {
		copied = await copyText(list?.folder ?? '');
		if (copied === 'manual' && folderEl) getSelection()?.selectAllChildren(folderEl);
	}

	const leaving = new AbortController();
	$effect(() => () => leaving.abort());
	let busy = $state(false);
	let result = $state('');
	let error = $state('');
	async function remove(which: 'all' | 'details') {
		const doomed = files.filter((f) => which === 'all' || f.details || f.opt_in);
		if (!doomed.length || busy) return;
		const count = formatCount(doomed.length);
		const size = formatBytes(doomed.reduce((n, f) => n + f.size, 0));
		const answer = await ask(
			{
				title:
					which === 'all'
						? m.settings_logging_delete_all_title()
						: m.settings_logging_delete_details_title(),
				body: [m.settings_logging_delete_what({ count, size }), m.settings_logging_delete_undo()],
				actions: [
					{ id: 'cancel', label: m.settings_form_cancel() },
					{
						id: 'delete',
						label: m.settings_logging_delete_button({ count, size }),
						variant: 'danger'
					}
				],
				initial: 'cancel'
			},
			leaving.signal
		);
		if (answer !== 'delete') return;
		busy = true;
		result = error = '';
		try {
			const done = await api.delete('/logs/files', { body: { which } });
			result = m.settings_logging_deleted({
				count: formatCount(done.deleted),
				size: formatBytes(done.bytes)
			});
			if (done.skipped.length)
				error = m.settings_logging_delete_skipped({
					count: formatCount(done.skipped.length),
					names: done.skipped.map((s) => s.name).join(', ')
				});
		} catch (e) {
			error = m.settings_logging_delete_failed({ message: (e as Error).message });
		} finally {
			busy = false;
			await load();
			void catalog.load();
		}
	}
	const detailFiles = $derived(files.filter((f) => f.details || f.opt_in).length);
</script>

<section class="flex flex-col gap-4" aria-labelledby="logging-files">
	<div>
		<h2 id="logging-files" class="text-body-lg font-semibold">{m.settings_logging_files()}</h2>
		<p class="text-muted-text">{m.settings_logging_files_lead()}</p>
	</div>
	{#if failed}
		<p role="alert" class="text-small text-error">{m.settings_logging_files_failed()}</p>
	{:else if !list}
		<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
	{:else}
		{#if list.folder}
			<div class="flex flex-wrap items-center gap-2">
				<span class="text-label">{m.settings_logging_files_folder()}</span>
				<span bind:this={folderEl} data-testid="log-folder"><Mono wrap>{list.folder}</Mono></span>
				<Button variant="secondary" dense onclick={copyFolder}>{m.settings_form_copy()}</Button>
				{#if copied}<span role="status" class="text-small text-muted-text"
						>{copied === 'copied' ? m.settings_form_copied() : m.settings_form_copy_failed()}</span
					>{/if}
			</div>
		{/if}
		{#if catalog.deployment === 'docker' || catalog.deployment === 'unraid'}
			<p class="text-small text-muted-text">{m.settings_logging_files_docker()}</p>
		{/if}
		{#if list.state !== 'on'}
			<p class="text-small text-muted-text" data-testid="log-files-state">
				{list.state === 'unavailable'
					? m.settings_logging_files_unavailable()
					: m.settings_logging_files_off()}
			</p>
		{/if}
		{#if !list.index_readable}
			<p class="text-small text-muted-text">{m.settings_logging_files_index()}</p>
		{/if}
		{#if files.length}
			<div class="flex flex-wrap items-center gap-4">
				<SegmentedControl
					dense
					label={m.settings_logging_files_format()}
					options={[
						{ value: 'text', label: m.settings_logging_files_text() },
						{ value: 'jsonl', label: m.settings_logging_files_jsonl() }
					]}
					bind:value={format}
				/>
				{#if anyDetails}
					<Checkbox label={m.settings_logging_files_redact()} bind:checked={redact} />
				{/if}
				<a class="underline" href={archiveHref} download data-testid="log-download-all"
					>{m.settings_logging_files_all()}</a
				>
			</div>
			<ul class="flex flex-col">
				{#each files as f (f.name)}
					<li
						class="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-base-300 py-2"
						data-testid="log-file-{f.name}"
					>
						<Mono>{f.name}</Mono>
						<span class="text-small" data-testid="log-file-size-{f.name}"
							>{formatBytes(f.size)}</span
						>
						<span class="text-small text-muted-text">{when(f.from)} – {when(f.to)}</span>
						{#if f.details || f.opt_in}<Badge variant="warning"
								>{m.settings_logging_files_details()}</Badge
							>{/if}
						<a
							class="ms-auto underline"
							href={fileHref(f)}
							download
							aria-label={m.settings_logging_files_download_one({ name: f.name })}
							>{m.settings_logging_files_download()}</a
						>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-muted-text">{m.settings_logging_files_none()}</p>
		{/if}
	{/if}
</section>

<section
	class="flex flex-col gap-3 rounded-box border border-error p-4"
	aria-labelledby="logging-danger"
>
	<h2 id="logging-danger" class="inline-flex items-center gap-2 text-body-lg font-semibold">
		<IconWarn class="size-5 text-error" aria-hidden="true" />{m.settings_logging_danger()}
	</h2>
	<p class="text-muted-text">{m.settings_logging_danger_lead()}</p>
	<div class="flex flex-wrap gap-3">
		<Button variant="danger" disabled={busy || !detailFiles} onclick={() => remove('details')}
			>{m.settings_logging_delete_details()}</Button
		>
		<Button variant="danger" disabled={busy || !files.length} onclick={() => remove('all')}
			>{m.settings_logging_delete_all()}</Button
		>
	</div>
	{#if result}<p role="status" class="text-small">{result}</p>{/if}
	{#if error}<p role="alert" class="text-small text-error">{error}</p>{/if}
</section>
