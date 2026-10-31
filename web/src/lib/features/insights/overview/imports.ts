// Import annotations (docs/decisions/2026-10-31-IMP-5.md): the text for an import's markers, with
// counts through `formatCount`. A null count is a dash, never 0.
import type { components } from '$lib/api/schema';
import { formatCount } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';

export type Annotation = components['schemas']['Annotation'];

const KINDS = new Set(['import_started', 'import_finished', 'import_removed']);
export const isImport = (a: Pick<Annotation, 'kind'>) => KINDS.has(a.kind);

/** The marker's text, or undefined for a kind that isn't an import's. */
export function importText(a: Annotation, locale?: string): string | undefined {
	const n = a.import;
	const c = (v: number | null | undefined) => formatCount(v ?? null, locale);
	switch (a.kind) {
		case 'import_started':
			return m.insights_import_started();
		case 'import_finished':
			return m.insights_import_finished({ new: c(n?.new), merged: c(n?.merged) });
		case 'import_removed':
			return m.insights_import_removed({ removed: c(n?.removed) });
		default:
			return undefined;
	}
}

/** An `import_finished` marker is in the range, so the index's change includes an import. */
export const hasFinishedImport = (list: readonly Pick<Annotation, 'kind'>[]) =>
	list.some((a) => a.kind === 'import_finished');

/** The "+ N imported" line: only for a count above zero (zero adds no clutter). */
export function plusImported(n: number | null | undefined, locale?: string): string | undefined {
	return n !== null && n !== undefined && n > 0
		? m.insights_import_plus({ count: formatCount(n, locale) })
		: undefined;
}
