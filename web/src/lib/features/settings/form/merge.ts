// The three-way merge for concurrent edits (§10.10.5), run on `409 stale_revision` and on a
// `settings` event for a section with staged edits. Pure. Per field, B = base, M = mine,
// T = theirs:
//   M = B (untouched)          → take T silently
//   T = B (only you changed)   → keep M, B := T
//   both changed, M = T        → clean
//   both changed, M ≠ T        → a conflict until resolved (Keep mine: B := T; Use theirs: M := T)
//   T now locked               → drop M, with a note on the field
//   gone from T                → drop the field, counted by the caller
// The section's revision becomes theirs. Nothing is re-saved.
import type { SectionValues, SettingValue } from './types';
import {
	equal,
	isLocked,
	normalize,
	RESET,
	type FieldModel,
	type SectionState,
	type Staged
} from './values';

export interface MergeOutcome {
	state: SectionState;
	/** Fields now in conflict. */
	conflicts: string[];
	/** Fields whose edit was dropped because they're now locked. */
	locked: string[];
	/** Fields that no longer exist on the server. */
	removed: string[];
	/** Untouched fields whose saved value changed. */
	updated: string[];
}

/** Is a staged value what theirs holds? A reset matches any value that isn't saved. */
function sameAs(mine: Staged, theirs: SettingValue): boolean {
	return mine === RESET ? theirs.source !== 'db' : equal(mine, theirs.value);
}

const changedSaved = (a: SettingValue, b: SettingValue) =>
	!equal(a.value, b.value) || a.source !== b.source;

export function merge(s: SectionState, theirs: SectionValues): MergeOutcome {
	const out: MergeOutcome = {
		state: s,
		conflicts: [],
		locked: [],
		removed: Object.keys(s.fields).filter((n) => !Object.hasOwn(theirs.fields, n)),
		updated: []
	};
	const fields: Record<string, FieldModel> = {};
	for (const [name, t] of Object.entries(theirs.fields)) {
		const f = s.fields[name];
		const untouched =
			!f || f.mine === undefined || (f.mine !== RESET && !f.conflict && equal(f.mine, f.base));
		if (untouched) {
			if (f && changedSaved(f.saved, t)) out.updated.push(name);
			fields[name] = f?.note ? { saved: t, note: f.note } : { saved: t };
			continue;
		}
		const mine = f.mine;
		if (isLocked(t)) {
			out.locked.push(name);
			fields[name] = { saved: t, note: 'locked' };
			continue;
		}
		if (f.conflict) {
			// Already shown as a conflict: the same theirs again changes nothing.
			if (sameAs(mine, t)) fields[name] = { saved: t };
			else {
				fields[name] = { ...f, saved: t, conflict: { mine, theirs: t.value } };
				out.conflicts.push(name);
			}
			continue;
		}
		if (equal(f.base, t.value)) {
			fields[name] = normalize({ saved: t, base: t.value, mine });
		} else if (sameAs(mine, t)) {
			fields[name] = { saved: t };
		} else {
			fields[name] = { saved: t, base: f.base, mine, conflict: { mine, theirs: t.value } };
			out.conflicts.push(name);
		}
	}
	out.state = {
		section: s.section,
		revision: theirs.revision,
		restart: theirs.restart_required,
		fields
	};
	return out;
}

/** Keep mine (B := T, the edit stays staged) or Use theirs (M := T, clean). */
export function resolve(s: SectionState, name: string, pick: 'mine' | 'theirs'): SectionState {
	const f = s.fields[name];
	if (!f?.conflict) return s;
	const next: FieldModel =
		pick === 'theirs'
			? { saved: f.saved }
			: normalize({ saved: f.saved, base: f.saved.value, mine: f.mine });
	return { ...s, fields: { ...s.fields, [name]: next } };
}

/** A section's saved values as `SectionValues`, to merge staged edits onto. */
export function asValues(s: SectionState): SectionValues {
	const fields: Record<string, SettingValue> = {};
	for (const [name, f] of Object.entries(s.fields)) fields[name] = f.saved;
	return { section: s.section, revision: s.revision, restart_required: s.restart, fields };
}
