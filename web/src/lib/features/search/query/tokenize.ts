// The query bar's highlighter (§10.1: "a tiny tokenizer for syntax highlighting"). It only colours
// the user's own text: the server's parser (`/parse`) is the single source of truth for what a
// query means, so this mirrors the Rust lexer's word boundaries (crates/search/src/query.rs `lex`)
// and nothing more. Pure, linear, never throws; the token texts concatenate back to the input.

export type TokenKind = 'field' | 'value' | 'range' | 'operator' | 'phrase' | 'text';

/** `text.slice(from, to)` is the token, in UTF-16 offsets. */
export interface Token {
	kind: TokenKind;
	from: number;
	to: number;
}

/**
 * Field names the server knows (`FIELDS` in crates/search/src/query.rs, the M4 swarm fields and
 * `sig:` included: the server knows them and says they are not available yet). An unknown `foo:`
 * is plain text, as it is to the server.
 */
export const FIELDS = new Set([
	'name',
	'path',
	'size',
	'files',
	'largest',
	'piece',
	'discovered',
	'age',
	'ext',
	'private',
	'v2',
	'hybrid',
	'imported',
	'backfill',
	'partial',
	'hash',
	'magnet',
	'title',
	'type',
	'res',
	'hdr',
	'codec',
	'audio',
	'lang',
	'group',
	'has',
	'resolved',
	'adult',
	'ambiguous',
	'hidden',
	'demoted',
	'flagged',
	'year',
	's',
	'e',
	'junk',
	'cat',
	'bytes',
	'file',
	'sig',
	'seeders',
	'leechers',
	'trackers',
	'dht',
	'seen',
	'time'
]);

// ASCII whitespace as Rust's `is_ascii_whitespace` (no vertical tab), and the word stops.
const SPACE = /[\t\n\f\r ]/;
const STOP = /[\t\n\f\r ()"|]/;
const RANGE = /[<>]|\.\./;

export function tokenize(text: string): Token[] {
	const out: Token[] = [];
	const push = (kind: TokenKind, from: number, to: number) => {
		if (to <= from) return;
		const last = out.at(-1);
		// Adjacent text (a run of spaces, then a word) is one token: fewer spans to render.
		if (last && last.kind === kind && kind === 'text' && last.to === from) last.to = to;
		else out.push({ kind, from, to });
	};
	/** A word or a field's value: a leading `~` and a trailing `*` are operators. */
	const term = (kind: TokenKind, from: number, to: number) => {
		if (to - from > 1 && text[from] === '~') {
			push('operator', from, from + 1);
			from++;
		}
		const star = to - from > 1 && text[to - 1] === '*';
		push(kind, from, star ? to - 1 : to);
		if (star) push('operator', to - 1, to);
	};

	let i = 0;
	const n = text.length;
	while (i < n) {
		const c = text.charAt(i);
		if (SPACE.test(c)) {
			let end = i + 1;
			while (end < n && SPACE.test(text.charAt(end))) end++;
			push('text', i, end);
			i = end;
		} else if (c === '(' || c === ')' || c === '|') {
			push('operator', i, i + 1);
			i++;
		} else if (c === '"') {
			const close = text.indexOf('"', i + 1);
			const end = close === -1 ? n : close + 1; // unterminated: runs to the end
			push('phrase', i, end);
			i = end;
		} else if ((c === '-' || c === '!') && i + 1 < n && !/[\t\n\f\r )|]/.test(text.charAt(i + 1))) {
			push('operator', i, i + 1); // negation only when a term follows
			i++;
		} else {
			let end = i + 1;
			while (end < n && !STOP.test(text.charAt(end))) end++;
			const word = text.slice(i, end);
			const colon = word.indexOf(':');
			if (word === 'OR' || word === 'AND' || word === 'NEAR' || word.startsWith('NEAR/')) {
				push('operator', i, end);
			} else if (colon > 0 && FIELDS.has(word.slice(0, colon))) {
				const at = i + colon + 1;
				push('field', i, at);
				term(RANGE.test(word.slice(colon + 1)) ? 'range' : 'value', at, end);
			} else {
				term('text', i, end);
			}
			i = end;
		}
	}
	return out;
}
