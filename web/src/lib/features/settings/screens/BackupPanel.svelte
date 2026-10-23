<script lang="ts">
	// General → Backup & portability (§10.10.6, lazy). Export saves `lodestar-settings.toml`. Import
	// reads a file the user picks, shows what it would change (a dry run), and applies it with the
	// dry run's revisions; Undo is offered for 10 s. The server parses and checks the file; this
	// only shows what it answered.
	import { ApiError } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import type { Component } from 'svelte';
	import { displayValue } from '../form/display';
	import { fieldLabel, loadSettingMessages } from '../form/messages';
	import type { FieldKey } from '../form/types';
	import { useSettings } from '../shell/state.svelte';
	import {
		MAX_IMPORT_BYTES,
		UNDO_MS,
		applyImport,
		download,
		dryRun,
		exportToml,
		skipText,
		undoImport,
		type ImportReport
	} from './backup';

	const settings = useSettings();
	const locale = $derived(currentLocale());

	// --- export ------------------------------------------------------------------------------
	let preferences = $state(false);
	let exporting = $state(false);
	let exportFailed = $state(false);
	async function exportNow() {
		exporting = true;
		exportFailed = false;
		try {
			download(await exportToml(preferences));
		} catch {
			exportFailed = true;
		} finally {
			exporting = false;
		}
	}

	// --- import ------------------------------------------------------------------------------
	let toml = $state<string>();
	let report = $state.raw<ImportReport>();
	let message = $state<{ kind: 'info' | 'error' | 'success'; text: string }>();
	let busy = $state(false);
	let applied = $state.raw<ImportReport>();
	let undoUntil = $state<number>();
	let undoTimer: ReturnType<typeof setTimeout> | undefined;
	let fileInput = $state<HTMLInputElement>();

	function reset() {
		toml = undefined;
		report = undefined;
		if (fileInput) fileInput.value = '';
	}
	function fail(e: unknown) {
		message = {
			kind: 'error',
			text: e instanceof ApiError && e.message ? e.message : m.settings_ui_import_failed()
		};
	}

	/** Messages for the sections a report names, so its fields read with their labels. */
	const ready = (r: ImportReport) => loadSettingMessages(r.changes.map((c) => c.section));

	async function pick(e: Event) {
		const file = (e.currentTarget as HTMLInputElement).files?.[0];
		message = undefined;
		applied = undefined;
		report = undefined;
		if (!file) return;
		if (file.size > MAX_IMPORT_BYTES) {
			message = { kind: 'error', text: m.settings_ui_import_too_large() };
			reset();
			return;
		}
		busy = true;
		try {
			toml = await file.text();
		} catch {
			message = { kind: 'error', text: m.settings_ui_import_unreadable() };
			busy = false;
			return;
		}
		await preview();
	}

	async function preview(note?: string) {
		busy = true;
		try {
			const r = await dryRun(toml!);
			await ready(r);
			report = r;
			message = note ? { kind: 'info', text: note } : undefined;
		} catch (e) {
			report = undefined;
			fail(e);
		} finally {
			busy = false;
		}
	}

	let AdultDialog =
		$state.raw<
			Component<{ confirm: import('$lib/features/filters/flows/adult.svelte').AdultConfirm }>
		>();
	let adult = $state.raw<import('$lib/features/filters/flows/adult.svelte').AdultConfirm>();
	async function confirmAdult(): Promise<boolean> {
		if (!adult) {
			const [{ createAdultConfirm }, dialog] = await Promise.all([
				import('$lib/features/filters/flows/adult.svelte'),
				import('$lib/features/filters/flows/AdultConfirmDialog.svelte')
			]);
			adult = createAdultConfirm();
			AdultDialog = dialog.default as never;
		}
		return adult.ask();
	}

	async function apply() {
		if (!toml || !report) return;
		busy = true;
		message = undefined;
		try {
			let done: ImportReport;
			try {
				done = await applyImport(toml, report.revisions);
			} catch (e) {
				// The file turns an adult setting on: ask once, then send the confirmation with it.
				if (e instanceof ApiError && e.code === 'adult_confirmation_required') {
					if (!(await confirmAdult())) return;
					done = await applyImport(toml, report.revisions, true);
				} else throw e;
			}
			await ready(done);
			applied = done;
			message = {
				kind: 'success',
				text: m.settings_ui_import_applied({ count: formatCount(done.changes.length, locale) })
			};
			undoUntil = Date.now() + UNDO_MS;
			clearTimeout(undoTimer);
			undoTimer = setTimeout(() => (undoUntil = undefined), UNDO_MS);
			reset();
		} catch (e) {
			if (e instanceof ApiError && e.code === 'stale_revision') {
				// Something changed after the dry run: compare again, and say so.
				await preview(m.settings_ui_import_stale());
			} else fail(e);
		} finally {
			busy = false;
		}
	}

	async function undo() {
		if (!applied) return;
		busy = true;
		try {
			await undoImport(applied);
			message = { kind: 'success', text: m.settings_ui_import_undone() };
		} catch (e) {
			message =
				e instanceof ApiError && (e.status === 409 || e.code === 'stale_revision')
					? { kind: 'error', text: m.settings_ui_import_undo_gone() }
					: { kind: 'error', text: m.settings_ui_import_failed() };
		} finally {
			clearTimeout(undoTimer);
			undoUntil = undefined;
			applied = undefined;
			busy = false;
		}
	}
	$effect(() => () => clearTimeout(undoTimer));

	function label(section: string, field: string) {
		const spec = settings.index?.spec(`${section}.${field}` as FieldKey);
		return spec ? fieldLabel(spec) : `${section}.${field}`;
	}
	function show(section: string, field: string, value: unknown) {
		const spec = settings.index?.spec(`${section}.${field}` as FieldKey);
		return spec ? displayValue(spec, value) : JSON.stringify(value);
	}
	void toast;
</script>

<div class="flex flex-col gap-6" data-testid="settings-backup">
	<div class="flex flex-col gap-3">
		<Toggle bind:checked={preferences} label={m.settings_ui_backup_prefs()} />
		<p class="text-small text-muted-text">{m.settings_ui_backup_prefs_help()}</p>
		<div>
			<Button loading={exporting} onclick={() => void exportNow()} data-testid="settings-export"
				>{m.settings_ui_backup_export()}</Button
			>
		</div>
		{#if exportFailed}<p role="alert">{m.settings_ui_backup_export_failed()}</p>{/if}
	</div>

	<div class="flex flex-col gap-3">
		<label class="text-small font-medium" for="settings-import-file"
			>{m.settings_ui_backup_file()}</label
		>
		<input
			id="settings-import-file"
			bind:this={fileInput}
			type="file"
			accept=".toml,application/toml,text/plain"
			class="import-file"
			onchange={pick}
			data-testid="settings-import-file"
		/>

		{#if message}
			<Banner kind={message.kind}>
				<span data-testid="settings-import-message">{message.text}</span>
				{#snippet action()}
					{#if undoUntil && applied}
						<Button dense onclick={() => void undo()} data-testid="settings-import-undo"
							>{m.settings_form_undo()}</Button
						>
					{/if}
				{/snippet}
			</Banner>
		{/if}

		{#if report}
			<section
				class="flex flex-col gap-3"
				aria-labelledby="import-diff"
				data-testid="settings-import-diff"
			>
				<h3 id="import-diff" class="text-body font-semibold" data-testid="settings-import-counts">
					{m.settings_ui_import_counts({
						changes: formatCount(report.changes.length, locale),
						skipped: formatCount(report.skipped.length, locale),
						unchanged: formatCount(report.unchanged, locale)
					})}
				</h3>
				{#if report.changes.length === 0}
					<p>{m.settings_ui_import_nothing()}</p>
				{:else}
					<ul class="flex flex-col gap-2">
						{#each report.changes as c (c.section + '.' + c.field)}
							<li data-testid="settings-import-change-{c.section}.{c.field}">
								{m.settings_ui_import_change({
									label: label(c.section, c.field),
									from: show(c.section, c.field, c.from),
									to: show(c.section, c.field, c.to)
								})}
								{#if c.restart_required}<span class="text-muted-text">· {m.setting_restart()}</span
									>{/if}
							</li>
						{/each}
					</ul>
				{/if}
				{#if report.skipped.length}
					<ul
						class="flex flex-col gap-1 text-small text-muted-text"
						data-testid="settings-import-skipped"
					>
						{#each report.skipped as s (s.key)}<li>{skipText(s)}</li>{/each}
					</ul>
				{/if}
				<div class="flex gap-2">
					<Button onclick={reset}>{m.settings_form_cancel()}</Button>
					<Button
						variant="primary"
						loading={busy}
						disabled={report.changes.length === 0}
						onclick={() => void apply()}
						data-testid="settings-import-apply">{m.settings_ui_import_apply()}</Button
					>
				</div>
			</section>
		{/if}
	</div>
</div>
{#if adult && AdultDialog}<AdultDialog confirm={adult} />{/if}

<style>
	.import-file {
		display: block;
		width: 100%;
		min-height: var(--control-height);
		padding: calc(var(--spacing) * 2);
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-field);
		background: var(--color-base-100);
		color: var(--color-base-content);
	}
</style>
