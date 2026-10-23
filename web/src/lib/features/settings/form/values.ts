// Per-section values and dirty tracking. Pure: every function returns new objects, so the
// store can keep sections in `$state.raw` and tests can hold the invariants without a DOM.
//
// Per field: `saved` (the server's last word), and while staged, `base` (the saved value the
// edit started from) and `mine` (the staged value, or `RESET`). A field is dirty iff `mine`
// differs from `saved`; a `mine` equal to `saved` is dropped, so the dirty count is exact.
import type { Batch } from './requests';
import type { FieldSpec, SectionValues, SettingValue } from './types';

/** A staged "remove the saved value": sent as `null`. */
export const RESET: unique symbol = Symbol('reset');
export type Staged = unknown;

export interface FieldModel {
	saved: SettingValue;
	base?: unknown;
	/** Absent when untouched. */
	mine?: Staged;
	conflict?: { mine: Staged; theirs: unknown };
	note?: string;
}

export interface SectionState {
	section: string;
	revision: number;
	restart: string[];
	fields: Record<string, FieldModel>;
}

/**
 * What the JSON fallback stages while its text doesn't parse: dirty (so leaving asks, and Save
 * can't send the last value that did parse), and always invalid. Never sent or stored.
 */
export class JsonDraft {
	constructor(readonly text: string) {}
}

/** Deep JSON equality: key-order-insensitive objects, `-0` equals `0`, never by reference. */
export function equal(a: unknown, b: unknown): boolean {
	if (a === b) return true; // includes 0 === -0
	if (typeof a !== typeof b || a === null || b === null) return false;
	if (typeof a === 'number') return Number.isNaN(a) && Number.isNaN(b as number);
	if (typeof a !== 'object') return false;
	if (Array.isArray(a) !== Array.isArray(b)) return false;
	if (Array.isArray(a)) {
		const bb = b as unknown[];
		return a.length === bb.length && a.every((v, i) => equal(v, bb[i]));
	}
	const ao = a as Record<string, unknown>;
	const bo = b as Record<string, unknown>;
	const ak = Object.keys(ao).filter((k) => ao[k] !== undefined);
	const bk = Object.keys(bo).filter((k) => bo[k] !== undefined);
	return ak.length === bk.length && ak.every((k) => Object.hasOwn(bo, k) && equal(ao[k], bo[k]));
}

export const isLocked = (v: SettingValue): boolean => v.read_only || v.source === 'env';

/** Does a staged value differ from what's saved? `RESET` does only while a value is saved. */
export function differs(mine: Staged, saved: SettingValue): boolean {
	if (mine === RESET) return saved.source === 'db';
	if (mine instanceof JsonDraft) return true; // text that doesn't parse is never a saved value
	return !equal(mine, saved.value);
}

export function isDirty(f: FieldModel): boolean {
	return f.mine !== undefined && differs(f.mine, f.saved);
}

/** Drops a `mine` that no longer differs from `saved` (and its base and conflict). */
export function normalize(f: FieldModel): FieldModel {
	if (f.mine === undefined || f.conflict || differs(f.mine, f.saved)) return f;
	return { saved: f.saved, ...(f.note ? { note: f.note } : {}) };
}

export function fromValues(v: SectionValues): SectionState {
	const fields: Record<string, FieldModel> = {};
	for (const [name, saved] of Object.entries(v.fields)) fields[name] = { saved };
	return { section: v.section, revision: v.revision, restart: v.restart_required, fields };
}

function withField(s: SectionState, name: string, f: FieldModel): SectionState {
	return { ...s, fields: { ...s.fields, [name]: f } };
}

/** Stages a value. Locked fields never take one. Setting the saved value back makes it clean. */
/**
 * Stages a value. Locked fields never take one. Setting the saved value back makes it clean,
 * except with `keep`: while the field's save is in flight `saved` is about to change, so an edit
 * back to it is a real change, judged against the response (`applySaved`).
 */
export function stage(
	s: SectionState,
	name: string,
	value: Staged,
	spec?: FieldSpec,
	keep = false
): SectionState {
	const f = s.fields[name];
	if (!f || isLocked(f.saved) || spec?.readOnly) return s;
	// An empty secret is no replacement, and a secret is never reset through Save: Remove runs on
	// its own, after a confirmation (§10.10.7).
	if (spec?.writeOnly && value === RESET) return s;
	if (spec?.writeOnly && (value === '' || value === null)) value = undefined;
	if (value === RESET && f.saved.source !== 'db') value = undefined;
	const next: FieldModel = {
		saved: f.saved,
		// Editing a field in conflict is choosing a new value over theirs: rebase on theirs.
		base: f.mine === undefined || f.conflict ? f.saved.value : f.base,
		mine: value
	};
	if (value === undefined) return withField(s, name, { saved: f.saved });
	return withField(s, name, keep ? next : normalize(next));
}

/** Drops every staged value and conflict; saved values stay. */
export function discardAll(s: SectionState): SectionState {
	const fields: Record<string, FieldModel> = {};
	for (const [name, f] of Object.entries(s.fields)) fields[name] = { saved: f.saved };
	return { ...s, fields };
}

export function dirtyNames(s: SectionState): string[] {
	return Object.keys(s.fields).filter((n) => isDirty(s.fields[n]!));
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
