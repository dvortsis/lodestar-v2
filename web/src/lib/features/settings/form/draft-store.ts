// Drafts' storage and writing them (§10.10.5, the format is in `draft.ts`): part of a section's
// first view, so an edit is kept from the first keystroke even if the edit chunk never loads.
// Reading and restoring one (`draft.ts`) load with the edit chunk.
import { devWarn } from './schema';
import type { FieldSpec } from './types';
import { dirtyNames, isLocked, JsonDraft, RESET, type SectionState } from './values';

export const DRAFT_PREFIX = 'lodestar.settings.draft.v1.';

export function storage(): Storage | undefined {
	try {
		return globalThis.sessionStorage ?? undefined;
	} catch {
		return undefined; // blocked (sandboxed frame, privacy settings)
	}
}

export function hasDraft(id: string): boolean {
	try {
		return storage()?.getItem(DRAFT_PREFIX + id) != null;
	} catch {
		return false;
	}
}

export function clearDraft(id: string): void {
	try {
		storage()?.removeItem(DRAFT_PREFIX + id);
	} catch {
		// Blocked: there's nothing stored to drop.
	}
}

export const DRAFT_MAX_BYTES = 64 * 1024;

export interface DraftSection {
	revision: number;
	base: Record<string, unknown>;
	mine: Record<string, unknown>;
	reset: string[];
}
export interface Draft {
	v: 1;
	at: number;
	sections: Record<string, DraftSection>;
}

export type SpecOf = (section: string, name: string) => FieldSpec | undefined;

/** Only fields a draft may hold: dirty, not locked, not secret, known to the form. */
export function draftOf(
	sections: Record<string, SectionState>,
	specOf: SpecOf,
	at: number
): Draft | null {
	const out: Record<string, DraftSection> = {};
	for (const [section, s] of Object.entries(sections)) {
		for (const name of dirtyNames(s)) {
			const f = s.fields[name]!;
			const spec = specOf(section, name);
			if (!spec || spec.writeOnly || spec.readOnly || isLocked(f.saved)) continue;
			if (f.mine instanceof JsonDraft) continue; // half-typed JSON: nothing to restore
			const d = (out[section] ??= { revision: s.revision, base: {}, mine: {}, reset: [] });
			d.base[name] = f.base;
			if (f.mine === RESET) d.reset.push(name);
			else d.mine[name] = f.mine;
		}
	}
	return Object.keys(out).length ? { v: 1, at, sections: out } : null;
}

/** Writes (or, with nothing to keep, removes) the draft. False when storage refused it. */
export function writeDraft(id: string, draft: Draft | null): boolean {
	const store = storage();
	if (!store) return false;
	try {
		if (!draft) {
			store.removeItem(DRAFT_PREFIX + id);
			return true;
		}
		const text = JSON.stringify(draft);
		if (new TextEncoder().encode(text).length > DRAFT_MAX_BYTES) {
			if (import.meta.env.DEV) devWarn(`the draft for "${id}" is over 64 KiB; it isn't kept`);
			store.removeItem(DRAFT_PREFIX + id);
			return false;
		}
		store.setItem(DRAFT_PREFIX + id, text);
		return true;
	} catch {
		return false; // full (QuotaExceededError) or blocked
	}
}
