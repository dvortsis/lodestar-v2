// The draft's storage key and the two calls a section's first view needs: is there a draft to
// offer, and drop it. The rest of drafts (`draft.ts`) loads with the first edit or an offer.
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
