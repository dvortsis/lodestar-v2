// What the save pipeline builds from staged values: the batch body, its keys, and applying a
// response. Pure (`values.ts` has the rest).
import type { Batch } from './requests';
import type { SectionValues } from './types';
import {
	dirtyNames,
	equal,
	normalize,
	RESET,
	type FieldModel,
	type SectionState,
	type Staged
} from './values';

/** Drops every staged value and conflict; saved values stay. */
export function discardAll(s: SectionState): SectionState {
	const fields: Record<string, FieldModel> = {};
	for (const [name, f] of Object.entries(s.fields)) fields[name] = { saved: f.saved };
	return { ...s, fields };
}

/** The wire value of a staged edit. */
export const wire = (mine: Staged): unknown => (mine === RESET ? null : mine);

/**
 * The save body: only dirty fields, each touched section once with its `if_revision`, `null`
 * for a reset. Untouched sections are never sent. `only` limits it to some fields.
 */
export function buildBatch(
	sections: Record<string, SectionState>,
	only?: (section: string, name: string) => boolean
): Batch {
	const batch: Batch = {};
	for (const [section, s] of Object.entries(sections)) {
		for (const name of dirtyNames(s)) {
			if (only && !only(section, name)) continue;
			batch[section] ??= { if_revision: s.revision };
			batch[section][name] = wire(s.fields[name]!.mine);
		}
	}
	return batch;
}

/** Fields of a batch, `section.field` (reserved keys left out). */
export function batchKeys(batch: Batch): string[] {
	return Object.entries(batch).flatMap(([section, patch]) =>
		Object.keys(patch)
			.filter((k) => k !== 'if_revision' && k !== 'confirm_adult')
			.map((k) => `${section}.${k}`)
	);
}

/**
 * Applies a save's response: saved values and the revision come from the server, and a field
 * is clean if what it holds now is what was sent. An edit made while the request was in flight
 * stays dirty against the new saved value.
 */
export function applySaved(
	s: SectionState,
	v: SectionValues,
	sent: Record<string, unknown>
): SectionState {
	const fields: Record<string, FieldModel> = {};
	for (const [name, saved] of Object.entries(v.fields)) {
		const f = s.fields[name];
		if (!f || f.mine === undefined) {
			fields[name] = { saved };
			continue;
		}
		const wasSent = Object.hasOwn(sent, name) && equal(wire(f.mine), sent[name]);
		fields[name] =
			wasSent && !f.conflict
				? { saved }
				: normalize({ ...f, saved, base: Object.hasOwn(sent, name) ? saved.value : f.base });
	}
	return { section: v.section, revision: v.revision, restart: v.restart_required, fields };
}
