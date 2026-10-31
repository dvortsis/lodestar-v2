import { describe, expect, it } from 'vitest';
import { formatTime, looksBinary, safe, seqOf } from './text';

describe('safe', () => {
	it('leaves markup as literal text and makes controls visible', () => {
		expect(safe('<script>alert(1)</script>')).toBe('<script>alert(1)</script>');
		expect(safe('a\nb\tc\0d')).toBe('a⏎b⇥c␀d');
		expect(safe('x\u001b[31my')).toBe('x\\u001b[31my');
	});
	it('makes bidirectional and invisible format characters visible', () => {
		expect(safe('a‮b​c')).toBe('a\\u202eb\\u200bc');
	});
	it('cuts long values on a character boundary', () => {
		expect(safe('x'.repeat(500), 10)).toBe(`${'x'.repeat(10)}…`);
		expect(safe('😀'.repeat(20), 5)).toBe(`${'😀'.repeat(5)}…`);
	});
	it('replaces binary-looking text with a note', () => {
		const bin = Array.from({ length: 64 }, (_, i) => String.fromCharCode(i % 8)).join('');
		expect(looksBinary(bin)).toBe(true);
		expect(safe(bin)).toBe('[binary data, 64 characters]');
	});
	it('shows numbers, flags and empty values', () => {
		expect(safe(5)).toBe('5');
		expect(safe(false)).toBe('false');
		expect(safe(null)).toBe('');
	});
});

describe('time and ids', () => {
	it('formats UTC exactly', () => {
		expect(formatTime('2026-10-31T10:05:07.042Z', true)).toBe('10:05:07.042');
		expect(formatTime('nonsense\u0000', true)).toBe('nonsense␀');
	});
	it('reads the sequence of an id', () => {
		expect(seqOf('00ff-12')).toBe(12);
		expect(seqOf('x')).toBeNull();
		expect(seqOf(null)).toBeNull();
	});
});
