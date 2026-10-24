import { describe, expect, it } from 'vitest';
import { showBidi } from './bidi';

describe('showBidi', () => {
	it('shows every override, embedding and isolating mark as a marker', () => {
		const controls = ['‪', '‫', '‬', '‭', '‮', '⁦', '⁧', '⁨', '⁩'];
		expect(showBidi(controls.join(''))).toBe(
			'<U+202A><U+202B><U+202C><U+202D><U+202E><U+2066><U+2067><U+2068><U+2069>'
		);
	});
	it('makes the spoofed name readable as it is', () => {
		expect(showBidi('invoice‮fdp.exe')).toBe('invoice<U+202E>fdp.exe');
	});
	it('leaves ordinary text, CJK and right-to-left letters alone', () => {
		for (const s of ['plain.mkv', '音楽/第2話.mkv', 'שלום.txt', 'مرحبا.mp3', ''])
			expect(showBidi(s)).toBe(s);
	});
});
