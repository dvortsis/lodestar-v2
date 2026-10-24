// What the save dialog shows for each failure (§10.14: what happened, then what to do). The
// server's code picks the field or the form; its `message` is shown only for query syntax, where
// it carries the parser's explanation (English only; zh for it is a follow-up).
import { ApiError } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';
import { MAX_SAVED_SEARCHES } from './api';
import { formatCount } from '$lib/format';

export interface SaveFailure {
	/** Shown on the Name or the Query field. */
	field?: 'name' | 'q';
	/** The field's message. */
	message?: string;
	/** The parser's explanation, under the field's message. */
	detail?: string;
	/** Characters of the query to select (the server's byte span, converted). */
	select?: [number, number];
	/** A form-level message, for everything that isn't one field's. */
	form?: string;
}

/** UTF-8 byte offsets in `text` to UTF-16 offsets for `setSelectionRange`. */
export function byteSpanToRange(text: string, start: number, end: number): [number, number] {
	let bytes = 0;
	let from = text.length;
	let to = text.length;
	let found = false;
	for (let i = 0; i < text.length;) {
		if (!found && bytes >= start) {
			from = i;
			found = true;
		}
		if (bytes >= end) {
			to = i;
			break;
		}
		const cp = text.codePointAt(i) ?? 0;
		bytes += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
		i += cp > 0xffff ? 2 : 1;
	}
	if (!found) from = text.length;
	return [from, Math.max(from, to)];
}

export function saveFailure(e: unknown, query: string, locale: string): SaveFailure {
	if (!(e instanceof ApiError)) return { form: m.alerts_err_generic() };
	switch (e.code) {
		case 'query_syntax': {
			const select =
				typeof e.start === 'number' && typeof e.end === 'number'
					? byteSpanToRange(query, e.start, e.end)
					: undefined;
			return { field: 'q', message: m.alerts_err_query(), detail: e.message || undefined, select };
		}
		case 'invalid_value':
			if (e.field === 'name') return { field: 'name', message: m.alerts_err_name() };
			if (e.field === 'q') return { field: 'q', message: m.alerts_err_query_value() };
			return { form: m.alerts_err_generic() };
		case 'too_many_saved_searches':
			return {
				form: m.alerts_err_too_many({ count: formatCount(MAX_SAVED_SEARCHES, locale) })
			};
		case 'network':
			return { form: m.alerts_err_network() };
		default:
			// q_and_ast, invalid_ast and unknown_sort mean a bug here; a 5xx is the server's.
			return { form: m.alerts_err_generic() };
	}
}
