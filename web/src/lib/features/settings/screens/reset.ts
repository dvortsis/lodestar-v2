// Reset all settings (§10.10.7, destructive): the saved values counted first, then one
// `PATCH /settings` with `null` for each, guarded by every section's revision. Preferences are
// per account and stay (runner answer 5); a secret is removed only through its own confirmation.
import { api } from '$lib/api/client';
import { patchBatch, type Batch } from '../form/requests';
import type { SettingsIndexView } from '../form/schema';

/** The section left out of Reset all. */
export const KEEP_SECTION = 'preferences';

export interface Saved {
	count: number;
	batch: Batch;
}

/** Every value saved in the app (`source === 'db'`) outside Preferences, and the batch that removes them. */
export async function savedSettings(
	index: SettingsIndexView,
	signal?: AbortSignal
): Promise<Saved> {
	const sections = index.sections.map((s) => s.section).filter((s) => s !== KEEP_SECTION);
	const values = await Promise.all(
		sections.map((section) => api.get('/settings/{section}', { path: { section }, signal }))
	);
	const batch: Batch = {};
	let count = 0;
	for (const v of values) {
		for (const [name, field] of Object.entries(v.fields)) {
			const spec = index.spec(`${v.section}.${name}`);
			if (field.source !== 'db' || field.read_only || spec?.writeOnly) continue;
			(batch[v.section] ??= { if_revision: v.revision })[name] = null;
			count++;
		}
	}
	return { count, batch };
}

export const resetAll = (saved: Saved) => patchBatch(saved.batch);
