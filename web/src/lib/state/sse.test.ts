import { describe, expect, it } from 'vitest';
import { SseParser, type SseItem } from './sse';

const wire =
	'retry: 5000\n\n' +
	': heartbeat\n\n' +
	'id: e1-1\nevent: torrents\ndata: {"added":2,\ndata: "max_id":9}\n\n' +
	'event: resync\ndata: {"reason":"lagged"}\n\n';

const expected: SseItem[] = [
	{ type: 'retry', ms: 5000 },
	{ type: 'message', id: 'e1-1', event: 'torrents', data: '{"added":2,\n"max_id":9}' },
	{ type: 'message', event: 'resync', data: '{"reason":"lagged"}' }
];

function parseChunks(chunks: string[]): SseItem[] {
	const p = new SseParser();
	return chunks.flatMap((c) => p.push(c));
}

describe('SseParser', () => {
	it('parses a whole stream', () => {
		expect(parseChunks([wire])).toEqual(expected);
	});

	it('is chunk-boundary independent: every single split point', () => {
		for (let i = 0; i <= wire.length; i++) {
			expect(parseChunks([wire.slice(0, i), wire.slice(i)])).toEqual(expected);
		}
	});

	it('survives one character at a time', () => {
		expect(parseChunks([...wire])).toEqual(expected);
	});

	it('handles CRLF and CR line endings, including a CRLF split across chunks', () => {
		for (const eol of ['\r\n', '\r']) {
			expect(parseChunks([wire.replaceAll('\n', eol)])).toEqual(expected);
		}
		const crlf = wire.replaceAll('\n', '\r\n');
		for (let i = 0; i <= crlf.length; i++) {
			expect(parseChunks([crlf.slice(0, i), crlf.slice(i)])).toEqual(expected);
		}
	});

	it('does not read the LF after a CR as an extra blank line', () => {
		const out = parseChunks(['data: a\r', '\ndata: b\r\n\r\n']);
		expect(out).toEqual([{ type: 'message', event: 'message', data: 'a\nb' }]);
	});

	it('handles fields without a value or without the space, and ignores unknown ones', () => {
		expect(parseChunks(['data:x\ndata\nfoo: bar\nevent:e\n\n'])).toEqual([
			{ type: 'message', event: 'e', data: 'x\n' }
		]);
	});

	it('does not dispatch a frame with no data', () => {
		expect(parseChunks(['event: x\nid: 1\n\n'])).toEqual([]);
	});

	it('keeps ids per frame: a frame without id has none', () => {
		const out = parseChunks(['id: 1\ndata: a\n\ndata: b\n\n']);
		expect(out.map((m) => (m.type === 'message' ? m.id : 'x'))).toEqual(['1', undefined]);
	});

	it('ignores a bad retry value', () => {
		expect(parseChunks(['retry: soon\n\n'])).toEqual([]);
	});

	it('holds an incomplete frame until its blank line', () => {
		const p = new SseParser();
		expect(p.push('data: a\n')).toEqual([]);
		expect(p.push('\n')).toHaveLength(1);
	});
});
