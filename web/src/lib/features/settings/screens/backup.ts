// Backup & portability (§10.10.6): export, the import's dry run and apply, Undo. Everything the
// server or a file sends is used only as the generated types say; a file is never read here beyond
// its size and text, which the server parses.
import { ApiError, api, errorFromResponse, handleUnauthorized, sessionOk } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { m } from '$lib/paraglide/messages.js';
import { patchBatch, type Batch } from '../form/requests';

export type ImportReport = components['schemas']['ImportReport'];
export type ImportChange = components['schemas']['ImportChange'];
export type ImportSkip = components['schemas']['ImportSkip'];

/** The server refuses a larger file (413); it isn't sent at all. */
export const MAX_IMPORT_BYTES = 64 * 1024;
/** How long Undo is offered after an import. */
export const UNDO_MS = 10_000;
export const EXPORT_FILE = 'lodestar-settings.toml';

/** `GET /settings/export` as text: the typed client reads JSON, and this answer is TOML. */
export async function exportToml(preferences: boolean): Promise<string> {
	const path = '/settings/export';
	let res: Response;
	try {
		res = await fetch(`/api/v1${path}?preferences=${preferences}`, {
			headers: { Accept: 'application/toml' },
			credentials: 'same-origin'
		});
	} catch (e) {
		throw new ApiError(0, 'network', e instanceof Error ? e.message : 'network error');
	}
	if (!res.ok) {
		const err = await errorFromResponse(res);
		handleUnauthorized(err, path);
		throw err;
	}
	sessionOk(path);
	return res.text();
}

/** Saves `text` as a file through a Blob link. */
export function download(text: string, name = EXPORT_FILE): void {
	const url = URL.createObjectURL(new Blob([text], { type: 'application/toml' }));
	const a = document.createElement('a');
	a.href = url;
	a.download = name;
	document.body.append(a);
	a.click();
	a.remove();
	// After the click has started the download.
	setTimeout(() => URL.revokeObjectURL(url), 0);
}

/** Only what the dry run would change; nothing is saved. */
export const dryRun = (toml: string) =>
	api.post('/settings/import', { query: { dry_run: true }, body: { toml } });

/** The real import, refused with 409 if a section changed since the dry run's `revisions`. */
export const applyImport = (
	toml: string,
	revisions: ImportReport['revisions'],
	confirmAdult = false
) =>
	api.post('/settings/import', {
		body: { toml, revisions, ...(confirmAdult ? { confirm_adult: true } : {}) }
	});

/**
 * One `PATCH /settings` that puts every change back: its `from` where that came from the app
 * (`db`), else `null`, which removes the saved value; guarded by the revisions after the import.
 */
export function undoBatch(report: Pick<ImportReport, 'changes' | 'revisions'>): Batch {
	const batch: Batch = {};
	for (const c of report.changes) {
		const section = (batch[c.section] ??= { if_revision: report.revisions[c.section] });
		section[c.field] = c.from_source === 'db' ? c.from : null;
	}
	return batch;
}

export const undoImport = (report: Pick<ImportReport, 'changes' | 'revisions'>) =>
	patchBatch(undoBatch(report));

/** A skipped key in words. */
export function skipText(skip: ImportSkip): string {
	switch (skip.reason) {
		case 'unknown':
			return m.settings_ui_import_skip_unknown({ key: skip.key });
		case 'read_only':
			return m.settings_ui_import_skip_read_only({ key: skip.key });
		case 'secret':
			return m.settings_ui_import_skip_secret({ key: skip.key });
		case 'set_by_environment':
			return m.settings_ui_import_skip_env({ key: skip.key, variable: skip.env ?? '' });
		default:
			return m.settings_ui_import_skip_other({ key: skip.key });
	}
}
