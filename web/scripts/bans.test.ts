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

	it('rejects spacing steps that are off the ramp', () => {
		expect(kinds('<div class="p-11">')).toEqual(['spacing']);
		expect(kinds('<div class="md:-mt-15 gap-x-16">')).toEqual(['spacing']);
		expect(kinds('<div class="m-3.5 w-64 h-96">')).toEqual(['spacing']);
		expect(kinds("const c = cond ? 'space-y-11' : 'top-20';")).toEqual(['spacing']);
		expect(kinds('<div class="!inset-x-15 size-11">')).toEqual(['spacing']);
	});

	it('rejects text size classes outside the type scale', () => {
		expect(kinds('<h1 class="text-h2 font-semibold">')).toEqual(['text']);
		expect(kinds('<p class="md:text-lg">')).toEqual(['text']);
		expect(kinds("const c = cond ? 'text-xs' : 'text-2xl';")).toEqual(['text']);
	});

	it('accepts the type scale, colours, alignment and wrapping', () => {
		for (const c of [
			'text-caption',
			'text-small',
			'text-body',
			'text-body-lg',
			'text-title',
			'text-display',
			'text-muted-text',
			'text-base-content',
			'text-error',
			'text-start',
			'text-end',
			'text-center',
			'text-balance',
			'text-ellipsis'
		])
			expect(kinds(`<p class="${c}">`)).toEqual([]);
	});

	it('accepts every step on the ramp', () => {
		expect(kinds('<div class="p-12">')).toEqual([]);
		expect(kinds('<div class="gap-0.5">')).toEqual([]);
		for (const n of [
			'0',
			'0.5',
			'1',
			'1.5',
			'2',
			'2.5',
			'3',
			'4',
			'5',
			'6',
			'7',
			'8',
			'9',
			'10',
			'12',
			'13',
			'14'
		]) {
			expect(kinds(`<div class="px-${n} -mt-${n} md:gap-y-${n} h-${n}">`)).toEqual([]);
		}
	});

	it('leaves keywords, fractions, layout tokens and other utilities alone', () => {
		expect(
			kinds('<div class="w-full h-px w-1/2 top-1/2 max-w-(--manuscript-width) w-(--rail-width)">')
		).toEqual([]);
		expect(
			kinds(
				'<div class="bg-primary/50 text-body leading-5 z-50 opacity-95 duration-150 grid-cols-12 line-clamp-3">'
			)
		).toEqual([]);
		expect(kinds('<div class="p-1.55">')).toEqual(['spacing']);
	});

	it('reports ban kinds independently', () => {
		expect(kinds('<div class="p-11 w-[13px]">').sort()).toEqual(['arbitrary', 'spacing']);
	});

	it('exempts the theme module and generated files', () => {
		expect(isExempt('src/lib/theme/riverstone.ts')).toBe(true);
		expect(isExempt('src/lib/api/schema.d.ts')).toBe(true);
		expect(isExempt('src/lib/ui/Button.svelte')).toBe(false);
	});
});
