import { m } from '$lib/paraglide/messages.js';

import { TYPE_LABELS as TYPE, ADULT_LABELS as ADULT } from '$lib/util/content-labels';

/** The label for a content type, or for an adult kind with `'adult'`. Unknown values show as sent. */
export function contentTypeLabel(value: string, kind: 'type' | 'adult' = 'type'): string {
	return (kind === 'adult' ? ADULT : TYPE)[value]?.() ?? value;
}

const VIA: Record<string, () => string> = {
	files: () => m.detail_via_files(),
	folder: () => m.detail_via_folder(),
	structure: () => m.detail_via_structure()
};

export const resolvedVia = (value: string): string => VIA[value]?.() ?? value;
