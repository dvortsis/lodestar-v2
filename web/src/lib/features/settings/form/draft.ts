// Drafts in `sessionStorage` (§10.10.5), one per screen: what's staged, so a reload or crash
// loses nothing. Never secrets, never locked fields, at most 64 KiB. Storage that's absent,
// full or blocked turns drafts off and nothing else; every call is guarded.
//
//   lodestar.settings.draft.v1.<id> = { v: 1, at, sections: { [section]: {
//     revision, base: { field: value }, mine: { field: value }, reset: [field] } } }
//
// A reset has no value, so it's listed in `reset` (with its base) rather than in `mine`.
//
// Writing one is in `draft-store.ts`, part of the first view so an edit is kept even when this
// file can't load; reading and restoring one are here.
import {
	clearDraft,
	DRAFT_PREFIX,
	storage,
	type Draft,
	type DraftSection,
	type SpecOf
} from './draft-store';
import { merge } from './merge';
import { validate } from './validate';
import { isLocked, RESET, type FieldModel, type SectionState, type Staged } from './values';

export * from './draft-store';

const isObj = (v: unknown): v is Record<string, unknown> =>
	typeof v === 'object' && v !== null && !Array.isArray(v);

/** The stored draft, shape-checked; a corrupt one is removed and reads as none. */
export function readDraft(id: string): Draft | null {
	const store = storage();
	if (!store) return null;
	let text: string | null;
	try {
		text = store.getItem(DRAFT_PREFIX + id);
	} catch {
		return null;
	}
	if (text === null) return null;
	try {
		const d: unknown = JSON.parse(text);
		if (!isObj(d) || d.v !== 1 || typeof d.at !== 'number' || !isObj(d.sections)) throw 0;
		const sections: Record<string, DraftSection> = {};
		for (const [section, s] of Object.entries(d.sections)) {
			if (!isObj(s) || !Number.isSafeInteger(s.revision) || !isObj(s.base) || !isObj(s.mine)) {
				throw 0;
			}
			const reset = Array.isArray(s.reset) ? s.reset.filter((r) => typeof r === 'string') : [];
			sections[section] = { revision: s.revision as number, base: s.base, mine: s.mine, reset };
		}
		return { v: 1, at: d.at, sections };
	} catch {
		clearDraft(id);
		return null;
	}
}

export interface RestorePlan {
	sections: Record<string, SectionState>;
	/** Fields the restore stages (conflicts included). */
	fields: number;
	/** Draft fields left out: unknown, now locked, secret or no longer valid. */
	dropped: number;
	conflicts: string[];
}

/**
 * Re-validates the draft against today's schema and values, then merges its edits onto the
 * current values as if they'd been staged at the draft's revision.
 */
export function planRestore(
	draft: Draft,
	current: Record<string, SectionState>,
	specOf: SpecOf
): RestorePlan {
	const plan: RestorePlan = { sections: { ...current }, fields: 0, dropped: 0, conflicts: [] };
	for (const [section, d] of Object.entries(draft.sections)) {
		const names = [...new Set([...Object.keys(d.mine), ...d.reset])];
		const cur = Object.hasOwn(current, section) ? current[section] : undefined;
		if (!cur) {
			plan.dropped += names.length;
			continue;
		}
		// Edits staged since the page loaded are newer than the draft's: they stay.
		const fields: Record<string, FieldModel> = { ...cur.fields };
		const restored: string[] = [];
		for (const name of names) {
			const spec = specOf(section, name);
			const f = Object.hasOwn(cur.fields, name) ? cur.fields[name] : undefined;
			const mine: Staged = d.reset.includes(name) ? RESET : d.mine[name];
			const ok =
				spec &&
				f &&
				!spec.writeOnly &&
				!spec.readOnly &&
				!isLocked(f.saved) &&
				Object.hasOwn(d.base, name) &&
				validate(spec, mine) === undefined;
			if (!ok) {
				plan.dropped++;
				continue;
			}
			if (f.mine !== undefined) continue; // edited since the page loaded: that edit stays
			restored.push(name);
			// As staged then: saved was the draft's base (a reset's base was a saved value).
			const source = mine === RESET ? 'db' : f.saved.source;
			fields[name] = {
				saved: { ...f.saved, value: d.base[name], source },
				base: d.base[name],
				mine
			};
		}
		const staged: SectionState = { ...cur, revision: d.revision, fields };
		const out = merge(staged, {
			section,
			revision: cur.revision,
			restart_required: cur.restart,
			fields: Object.fromEntries(Object.entries(cur.fields).map(([n, f]) => [n, f.saved]))
		});
		plan.sections[section] = out.state;
		plan.conflicts.push(...out.conflicts.map((n) => `${section}.${n}`));
		// Only what the draft brings back: not edits made since, nor ones the merge found settled.
		plan.fields += restored.filter((n) => out.state.fields[n]?.mine !== undefined).length;
	}
	return plan;
}
