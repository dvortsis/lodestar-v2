<script lang="ts">
	// Log files (§10.10.12, §12.1): the list with name, size, time range and a "Contains torrent
	// details" badge; Download per file and Download all (zip), as text or JSON lines, with
	// "Remove torrent details" on by default; the folder with Copy; on Docker, where the console log
	// is. Then the Danger zone: Delete log files (all, or only those with torrent details), each
	// behind a dialog with the count and size, focus on Cancel.
	import { tick } from 'svelte';
	import IconWarn from '~icons/ph/warning';
	import { api, isAbort } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { formatBytes, formatCount, formatDateTime } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
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

	let listing = $state.raw<Files>();
	let failed = $state(false);
	let format = $state<'text' | 'jsonl'>('text');
	// On by default (§12.1), and on again whenever the list is read: unticking it is for the
	// downloads the user makes now. It matters only for files that have torrent details.
	let redact = $state(true);
	async function load() {
		try {
			listing = await api.get('/logs/files');
			failed = false;
			redact = true;
		} catch (e) {
			if (isAbort(e)) return;
			// A stale list would put the wrong count and size in the next delete dialog.
			listing = undefined;
			failed = true;
		}
	}
	void load();

	const locale = $derived(currentLocale());
	const files = $derived(listing?.files ?? []);
	const hasDetails = (f: File) => f.details || f.opt_in;
	const anyDetails = $derived(files.some(hasDetails));
	const query = (details: boolean) => `?format=${format}${details ? `&redact=${redact}` : ''}`;
	const fileHref = (f: File) =>
		`/api/v1/logs/files/${encodeURIComponent(f.name)}${query(hasDetails(f))}`;
	const archiveHref = $derived(`/api/v1/logs/archive.zip${query(anyDetails)}`);
	const when = (s: string | null) => formatDateTime(s ? Date.parse(s) / 1000 : null, locale);
	const bytes = (n: number) => formatBytes(n, locale);
	const count = (n: number) => formatCount(n, locale);
	// Why nothing is listed or written, in words the user can act on.
	const STATE: Record<string, () => string> = {
		off: m.settings_logging_files_off,
		stopped: m.settings_logging_files_stopped,
		not_opened: m.settings_logging_files_not_opened,
		not_a_folder: m.settings_logging_files_not_a_folder,
		not_private: m.settings_logging_files_not_private,
		unreadable: m.settings_logging_files_unavailable
	};
	const stateText = $derived(
		!listing || listing.state === 'on'
			? ''
			: (
					STATE[
						listing.state === 'unavailable' ? (listing.reason ?? 'unreadable') : listing.state
					] ?? m.settings_logging_files_unavailable
				)()
	);

	let copied = $state('');
	let folderEl = $state<HTMLElement>();
	async function copyFolder() {
		const done = await copyText(listing?.folder ?? '');
		copied = done === 'copied' ? m.settings_form_copied() : m.settings_form_copy_failed();
		if (done === 'manual' && folderEl) getSelection()?.selectAllChildren(folderEl);
	}

	const leaving = new AbortController();
	$effect(() => () => leaving.abort());
	let busy = $state(false);
	let result = $state('');
	let error = $state('');
	let resultEl = $state<HTMLElement>();
	const detailFiles = $derived(files.filter(hasDetails));
	async function remove(which: 'all' | 'details') {
		const doomed = which === 'all' ? files : detailFiles;
		if (!doomed.length || busy) return;
		const n = count(doomed.length);
		const size = bytes(doomed.reduce((s, f) => s + f.size, 0));
		const answer = await ask(
			{
				title:
					which === 'all'
						? m.settings_logging_delete_all_title()
						: m.settings_logging_delete_details_title(),
				body: [
					m.settings_logging_delete_what({ count: n, size }),
					m.settings_logging_delete_undo()
				],
				actions: [
					{ id: 'cancel', label: m.settings_form_cancel() },
					{
						id: 'delete',
						label: m.settings_logging_delete_button({ count: n, size }),
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
			result = m.settings_logging_deleted({ count: count(done.deleted), size: bytes(done.bytes) });
			if (done.skipped.length)
				error = m.settings_logging_delete_skipped({
					count: count(done.skipped.length),
					names: done.skipped.map((s) => s.name).join(', ')
				});
		} catch (e) {
			error = m.settings_logging_delete_failed({ message: (e as Error).message });
		} finally {
			busy = false;
			await load();
			void catalog.load();
			// The button may have nothing left to delete: keep focus on the page, at the result.
			await tick();
			if (!document.activeElement || document.activeElement === document.body) resultEl?.focus();
		}
	}
</script>

<section class="flex flex-col gap-4" aria-labelledby="logging-files">
	<div>
		<h2 id="logging-files" class="text-body-lg font-semibold">{m.settings_logging_files()}</h2>
		<p class="text-muted-text">{m.settings_logging_files_lead()}</p>
	</div>
	{#if failed}
		<p role="alert" class="text-small text-error">{m.settings_logging_files_failed()}</p>
	{:else if !listing}
		<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
	{:else}
		{#if listing.folder}
			<div class="flex flex-wrap items-center gap-2">
				<span class="text-label">{m.settings_logging_files_folder()}</span>
				<span bind:this={folderEl} data-testid="log-folder"><Mono wrap>{listing.folder}</Mono></span
				>
				<Button
					variant="secondary"
					dense
					aria-label={m.settings_logging_files_copy_folder()}
					onclick={copyFolder}>{m.settings_form_copy()}</Button
				>
				<span role="status" class="text-small text-muted-text">{copied}</span>
			</div>
		{/if}
		{#if catalog.deployment === 'docker' || catalog.deployment === 'unraid'}
			<p class="text-small text-muted-text" data-testid="log-files-docker">
				{m.settings_logging_files_docker()}
			</p>
		{/if}
		{#if stateText}
			<p class="text-small text-muted-text" data-testid="log-files-state">{stateText}</p>
		{/if}
		{#if !listing.index_readable}
			<p class="text-small text-muted-text" data-testid="log-files-index">
				{m.settings_logging_files_index()}
			</p>
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
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- an API download, not a page -->
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
						<span class="text-small" data-testid="log-file-size-{f.name}">{bytes(f.size)}</span>
						<span class="text-small text-muted-text" data-testid="log-file-time-{f.name}"
							>{when(f.from)} – {when(f.to)}</span
						>
						{#if hasDetails(f)}<Badge variant="warning">{m.settings_logging_files_details()}</Badge
							>{/if}
						<!-- eslint-disable svelte/no-navigation-without-resolve -- an API download, not a page -->
						<a
							class="ms-auto underline"
							href={fileHref(f)}
							download
							aria-label={m.settings_logging_files_download_one({ name: f.name })}
							>{m.settings_logging_files_download()}</a
						>
						<!-- eslint-enable svelte/no-navigation-without-resolve -->
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
		<Button
			variant="danger"
			loading={busy}
			disabled={failed || !detailFiles.length}
			onclick={() => remove('details')}>{m.settings_logging_delete_details()}</Button
		>
		<Button
			variant="danger"
			loading={busy}
			disabled={failed || !files.length}
			onclick={() => remove('all')}>{m.settings_logging_delete_all()}</Button
		>
	</div>
	<p bind:this={resultEl} tabindex="-1" role="status" class="text-small">{result}</p>
	{#if error}<p role="alert" class="text-small text-error">{error}</p>{/if}
</section>
