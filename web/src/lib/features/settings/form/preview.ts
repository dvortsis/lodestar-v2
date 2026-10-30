// Previews (§10.10.5): the dirty batch sent to POST /settings/preview as the user types, plus
// fields at their automatic value so their effective value shows. Loaded during the first
// preview's debounce.
import { ApiError, isAbort } from '$lib/api/client';
import type { Engine } from './form.svelte';
import { errorKey, previewBatch, type Batch } from './requests';
import { buildBatch } from './batch';
import { isLocked, JsonDraft } from './values';

function previewBody(e: Engine): Batch {
	const batch = buildBatch(
		e.sections,
		(s, n) =>
			e.inForm(s, n) &&
			!e.specOf(s, n)!.writeOnly &&
			!(e.sections[s]?.fields[n]?.mine instanceof JsonDraft)
	);
	for (const spec of e.specs) {
		if (!spec.preview || spec.auto === undefined || spec.writeOnly) continue;
		const s = e.sections[spec.section];
		const f = s?.fields[spec.name];
		if (!s || !f || f.mine !== undefined || isLocked(f.saved) || spec.readOnly) continue;
		if (f.saved.value !== spec.auto) continue;
		(batch[spec.section] ??= { if_revision: s.revision })[spec.name] = f.saved.value;
	}
	return batch;
}

export async function runPreview(e: Engine) {
	if (e.destroyed) return;
	const body = previewBody(e);
	if (Object.keys(body).length === 0) {
		e.previewSlot.cancel();
		e.preview = null;
		e.previewErrors = {};
		return;
	}
	try {
		e.preview = await e.previewSlot.run((signal) => previewBatch(body, signal));
		e.previewErrors = {};
	} catch (err) {
		if (isAbort(err)) return;
		// A refusal shows on its field but never blocks typing or Save.
		const key = err instanceof ApiError ? errorKey(e, err, body) : undefined;
		e.previewErrors = key && err instanceof ApiError ? { [key]: err.message } : {};
	}
}

export { default as FieldPreview } from './FieldPreview.svelte';
export { automaticNumber } from './display';
