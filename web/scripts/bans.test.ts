import { describe, expect, it } from 'vitest';
import { findViolations, isExempt, stripComments } from './bans.mjs';

const kinds = (src: string) => findViolations(src).map((v) => v.kind);

describe('lint bans', () => {
	it('flags hex colours', () => {
		expect(kinds('<div style="color: #9381B6">')).toEqual(['hex']);
		expect(kinds("const c = '#abc';")).toEqual(['hex']);
		expect(kinds('fill: #11223344;')).toEqual(['hex']);
	});

	it('flags Tailwind arbitrary values, including important and variant prefixes', () => {
		expect(kinds('<div class="w-[13px]">')).toEqual(['arbitrary']);
		expect(kinds('<div class="!mt-[2px]">')).toEqual(['arbitrary']);
		expect(kinds('<div class="md:hover:-top-[3px]">')).toEqual(['arbitrary']);
		expect(kinds('<div class="[mask-type:alpha]">')).toEqual(['arbitrary']);
	});

	it('flags arbitrary hex colours as both', () => {
		expect(kinds('<div class="bg-[#fff]">').sort()).toEqual(['arbitrary', 'hex']);
	});

	it('ignores issue references in comments', () => {
		expect(kinds('// issue #123')).toEqual([]);
		expect(kinds('const x = 1; // see #123')).toEqual([]);
		expect(kinds('/* fix #abc */')).toEqual([]);
		expect(kinds('<!-- bg-[#fff] -->')).toEqual([]);
	});

	it('keeps line numbers when stripping comments', () => {
		const src = '/* a\nb */\nconst c = "#fff";';
		expect(stripComments(src).split('\n')).toHaveLength(3);
		expect(findViolations(src)).toEqual([{ line: 3, kind: 'hex' }]);
	});

	it('ignores URL fragments and ids that look like hex', () => {
		expect(kinds('<a href="#add">Add</a>')).toEqual([]);
		expect(kinds('<label for="bad">')).toEqual([]);
		expect(kinds('<div id="fee" aria-describedby="#abc">')).toEqual([]);
		expect(kinds('<a href="#add" style="color: #fff">')).toEqual(['hex']);
	});

	it('does not flag plain class names or https URLs', () => {
		expect(kinds('<div class="w-4 md:p-2 hover:bg-primary">')).toEqual([]);
		expect(kinds("const u = 'https://example.com/a';")).toEqual([]);
	});

	it('exempts the theme module and generated files', () => {
		expect(isExempt('src/lib/theme/riverstone.ts')).toBe(true);
		expect(isExempt('src/lib/api/schema.d.ts')).toBe(true);
		expect(isExempt('src/lib/ui/Button.svelte')).toBe(false);
	});
});
