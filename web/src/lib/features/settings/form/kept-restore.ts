// Restoring a draft kept across a sign-in (X-session-draft-keep), on demand: the form loads this
// only when a kept-for-sign-in mark is in `sessionStorage` (one read), so neither a first view nor
// the edit chunk carries it. The note it hands the form (`KeptNote`) says the changes aren't saved yet.
import { KEPT_KEY, storage } from './draft-scope';
import type { Engine } from './form.svelte';
import type { Component } from 'svelte';
import KeptNote from './KeptNote.svelte';

/** A kept mark older than this isn't honoured (the draft is still offered as usual). */
export const KEPT_MAX_AGE_MS = 24 * 60 * 60 * 1000;

/**
 * Whether `id`'s draft was kept (by `signed-out.ts`) for a sign-in that has now returned to
 * `path`. A mark naming this form is removed, matched or not, so it's honoured once, on the
 * screen it came from; a bad or stale one reads as none. A later session end marks afresh.
 */
export function takeKept(id: string, path: string, now: number): boolean {
	try {
		const store = storage();
		const text = store?.getItem(KEPT_KEY);
		if (!store || !text) return false;
		let k: { id?: unknown; path?: unknown; at?: unknown } | null = null;
		try {
			k = text.length <= 4096 ? JSON.parse(text) : null;
		} catch {
			// unreadable: removed below
		}
		if (typeof k?.id === 'string' && k.id !== id) return false; // another screen's
		store.removeItem(KEPT_KEY);
		const age = typeof k?.at === 'number' ? now - k.at : -1;
		return k?.path === path && age >= 0 && age <= KEPT_MAX_AGE_MS;
	} catch {
		return false; // blocked
	}
}

/**
 * Offers the stored draft (`offer`, the edit chunk's `offerDraft`), then, when it was kept for the
 * sign-in that returned here, restores it at once and shows the note instead of the offer. The
 * mark is consumed either way, so it never lingers for a later draft.
 */
export function offerKept(
	e: Engine,
	offer: (e: Engine) => void,
	restore: (e: Engine) => void
): void {
	const kept = takeKept(e.id, globalThis.location?.pathname ?? '', Date.now());
	offer(e);
	if (!kept || !e.draft) return;
	// What the offer would have said: fields that no longer apply (now locked, or gone) are left out.
	const dropped = e.draft.dropped;
	restore(e);
	e.kept = dropped
		? (((internals, props) => KeptNote(internals, { ...props, dropped })) as Component)
		: KeptNote;
}
