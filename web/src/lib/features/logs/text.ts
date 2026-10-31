// Log text is untrusted (§12.1): a torrent name, a URL or a peer's string can say anything. Every
// string reaches the page through `safe()`, and the page only ever renders it as text (never
// `{@html}`). Control characters become visible escapes, bidirectional and invisible format
// characters too (a name must not reorder the line around it), and a long value is cut.
import type { components } from '$lib/api/schema';

export type LogLine = components['schemas']['LogLine'];
export type LogField = components['schemas']['LogField'];
export type LogLevel = components['schemas']['LogLevel'];
export type LogComponent = components['schemas']['LogComponent'];

/** A one-line row shows at most this much of a message; the expanded view, `FIELD_MAX`. */
export const ROW_MAX = 400;
export const FIELD_MAX = 2000;

// C0 and C1 controls, DEL, bidi controls, zero-width and other invisible format characters, the
// line and paragraph separators, and lone surrogates.
const UNSAFE = new RegExp(
	'[\\u0000-\\u001f\\u007f-\\u009f\\u00ad\\u061c\\u180e\\u200b-\\u200f\\u2028-\\u202e\\u2060-\\u206f\\ufeff\\ufff9-\\ufffb]' +
		'|[\\ud800-\\udbff](?![\\udc00-\\udfff])|(?<![\\ud800-\\udbff])[\\udc00-\\udfff]',
	'g'
);

const NAMED: Record<string, string> = { '\n': '⏎', '\r': '␍', '\t': '⇥', '\0': '␀' };

function visible(ch: string): string {
	return NAMED[ch] ?? `\\u${ch.charCodeAt(0).toString(16).padStart(4, '0')}`;
}

/** True when the text is mostly not text (binary, a dump): shown as a size note, not a wall. */
export function looksBinary(s: string): boolean {
	if (s.length < 32) return false;
	const sample = s.length > 512 ? s.slice(0, 512) : s;
	let bad = 0;
	for (const ch of sample) {
		const c = ch.codePointAt(0)!;
		if ((c < 0x20 && c !== 9 && c !== 10 && c !== 13) || c === 0xfffd) bad++;
	}
	return bad / sample.length > 0.2;
}

/** The text with unsafe characters made visible and cut to `max` characters (code points) with `…`. */
export function safe(value: string | number | boolean | null | undefined, max = ROW_MAX): string {
	if (value === null || value === undefined) return '';
	let s = String(value);
	if (s.length > max * 2 + 2) s = s.slice(0, max * 2 + 2); // bound the work before the regex runs
	const binary = looksBinary(s);
	const out = s.replace(UNSAFE, visible);
	if (binary) return `[binary data, ${String(value).length} characters]`;
	const chars = Array.from(out);
	return chars.length > max ? `${chars.slice(0, max).join('')}…` : out;
}

const pad = (n: number, w = 2) => String(n).padStart(w, '0');

/** `HH:MM:SS.mmm` in local time or UTC, from the RFC 3339 `ts`; the raw text when it won't parse. */
export function formatTime(ts: string, utc: boolean): string {
	const d = new Date(ts);
	if (Number.isNaN(d.getTime())) return safe(ts, 32);
	return utc
		? `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}.${pad(d.getUTCMilliseconds(), 3)}`
		: `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

const LEVELS = new Set(['error', 'warn', 'info', 'debug', 'trace']);
const synthetic = new WeakMap<object, string>();
let syntheticN = 0;

/** A stable key: the line's id, else one made once per line object (an index shifts on trim). */
export function lineKey(line: LogLine): string {
	if (line.id) return line.id;
	let k = synthetic.get(line);
	if (!k) synthetic.set(line, (k = `~${++syntheticN}`));
	return k;
}

/** On ingest: an unknown level reads as info, and a missing field list as empty. */
export function normalize(line: LogLine): LogLine {
	if (LEVELS.has(line.level) && Array.isArray(line.fields)) return line;
	const out = {
		...line,
		level: LEVELS.has(line.level) ? line.level : 'info',
		fields: Array.isArray(line.fields) ? line.fields : []
	} as LogLine;
	if (!line.id) synthetic.set(out, lineKey(line));
	return out;
}

/** The `<epoch>-<seq>` sequence part, or null. */
export function seqOf(id: string | null | undefined): number | null {
	const m = /^[0-9a-f]+-(\d+)$/.exec(id ?? '');
	return m ? Number(m[1]) : null;
}

/** The line as the row shows it: also what Copy and Download write, and what the keys filter reads. */
export function lineText(line: LogLine, utc: boolean, max = ROW_MAX): string {
	const rep = line.repeated
		? ` (repeated ${line.repeated.count} times in ${line.repeated.seconds} s)`
		: '';
	return `${formatTime(line.ts, utc)} ${line.level.toUpperCase()} ${line.component} ${safe(line.msg || line.event || line.target, max)}${rep}`;
}

/** Applies a `purge` frame: kinds turned off. Values read `[hidden]`; an opt-in's lines go. */
export function purge(lines: LogLine[], kinds: string[], through: string): LogLine[] {
	const last = lines.findLastIndex((l) => l.id === through);
	const cut = last < 0 ? lines.length - 1 : last;
	const off = new Set(kinds);
	const out: LogLine[] = [];
	for (const [i, l] of lines.entries()) {
		if (i > cut) out.push(l);
		else if (l.opt_in && off.has(l.opt_in)) continue;
		else if (l.fields.some((f) => off.has(f.kind))) {
			const copy = {
				...l,
				fields: l.fields.map((f) => (off.has(f.kind) ? { ...f, value: '[hidden]' } : f))
			};
			if (!l.id) synthetic.set(copy, lineKey(l));
			out.push(copy);
		} else out.push(l);
	}
	return out;
}
