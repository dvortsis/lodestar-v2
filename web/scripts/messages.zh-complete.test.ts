// Every en message has a zh message with the same placeholders, in the real catalogues.
// The generator only warns about a missing zh key, so this test is what keeps the gap from
// coming back silently (H-zh: the zh catalogue was 473 keys short).
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { discover, merge, placeholders } from './messages.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const result = merge(discover(root));

const names = (text: string) =>
	[...new Set((placeholders(text) as { names: string[] }).names)].sort().join(',');

describe('zh message catalogue', () => {
	it('has no catalogue errors', () => {
		expect(result.errors).toEqual([]);
	});

	it('has a zh message for every en key', () => {
		const missing = Object.keys(result.messages.en).filter((k) => !(k in result.messages.zh));
		expect(missing).toEqual([]);
	});

	it('uses the same placeholders in zh as in en', () => {
		const mismatched = Object.keys(result.messages.zh).filter(
			(k) => names(result.messages.en[k] ?? '') !== names(result.messages.zh[k] ?? '')
		);
		expect(mismatched).toEqual([]);
	});

	it('covers every en key (coverage 1)', () => {
		expect(result.coverage.zh).toBe(1);
	});
});
