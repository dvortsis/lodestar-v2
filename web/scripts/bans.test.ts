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
		expect(kinds('<div class="medium:hover:-top-[3px]">')).toEqual(['arbitrary']);
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
		expect(kinds('<div class="w-4 medium:p-2 hover:bg-primary">')).toEqual([]);
		expect(kinds("const u = 'https://example.com/a';")).toEqual([]);
	});

	it('rejects spacing steps that are off the ramp', () => {
		expect(kinds('<div class="p-11">')).toEqual(['spacing']);
		expect(kinds('<div class="medium:-mt-15 gap-x-16">')).toEqual(['spacing']);
		expect(kinds('<div class="m-3.5 w-64 h-96">')).toEqual(['spacing']);
		expect(kinds("const c = cond ? 'space-y-11' : 'top-20';")).toEqual(['spacing']);
		expect(kinds('<div class="!inset-x-15 size-11">')).toEqual(['spacing']);
	});

	it('rejects text size classes outside the type scale', () => {
		expect(kinds('<h1 class="text-h2 font-semibold">')).toEqual(['text']);
		expect(kinds('<p class="medium:text-lg">')).toEqual(['text']);
		expect(kinds('<span class="text-label font-semibold">')).toEqual(['text']);
		expect(kinds('<span class="text-small">')).toEqual([]);
		expect(kinds("const c = cond ? 'text-xs' : 'text-2xl';")).toEqual(['text']);
		expect(kinds('@apply font-semibold text-lg;')).toEqual(['text']);
		expect(kinds('<p class:text-xl={big}>')).toEqual(['text']);
		expect(kinds('<p class="text-sm/6">')).toEqual(['text']);
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
			expect(kinds(`<div class="px-${n} -mt-${n} medium:gap-y-${n} h-${n}">`)).toEqual([]);
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

	it('flags the default breakpoint variants the theme removed', () => {
		for (const v of ['sm', 'md', 'lg', 'xl', '2xl'])
			expect(kinds(`<div class="grid ${v}:grid-cols-2">`)).toEqual(['breakpoint']);
		expect(kinds('<div class="p-4 xl:p-6">')).toEqual(['breakpoint']);
		expect(kinds('<div class="hover:md:underline">')).toEqual(['breakpoint']);
		expect(kinds('<div class="md:!p-2">')).toEqual(['breakpoint']);
		expect(kinds("const c = wide ? 'lg:flex' : 'block';")).toEqual(['breakpoint']);
		expect(kinds('<div class={`gap-2 sm:gap-4`}>')).toEqual(['breakpoint']);
	});

	it('accepts the theme breakpoints and code that only looks like a variant', () => {
		for (const v of ['medium', 'large', 'x-large', 'xx-large', 'xxx-large'])
			expect(kinds(`<div class="${v}:grid-cols-2">`)).toEqual([]);
		expect(kinds('const sizes = { sm: 1, md: 2, lg: 3 };')).toEqual([]);
		expect(kinds('const s = {sm:1,md:2};')).toEqual([]);
		expect(kinds('function pad(md: string, lg?: number) {}')).toEqual([]);
		expect(kinds('const w = wide ? lg : sm;')).toEqual([]);
		expect(kinds("const label = 'Size: md';")).toEqual([]);
		expect(kinds('// sm:grid-cols-2 in a comment')).toEqual([]);
	});

	it('flags Phosphor imports without a weight', () => {
		expect(kinds("import IconGear from '~icons/ph/gear';")).toEqual(['icon']);
		expect(kinds('import IconX from "~icons/ph/x-circle";')).toEqual(['icon']);
		expect(kinds("export { default as bell } from '~icons/ph/bell';")).toEqual(['icon']);
		expect(kinds("const load = () => import('~icons/ph/warning');")).toEqual(['icon']);
		expect(kinds("import IconThin from '~icons/ph/gear-thin';")).toEqual(['icon']);
		expect(kinds("import IconLight from '~icons/ph/gear-light';")).toEqual(['icon']);
		expect(kinds("import IconDuo from '~icons/ph/gear-duotone';")).toEqual(['icon']);
	});

	it('accepts Bold and Fill Phosphor imports', () => {
		expect(kinds("import IconGear from '~icons/ph/gear-bold';")).toEqual([]);
		expect(kinds("import IconStar from '~icons/ph/star-fill';")).toEqual([]);
		expect(kinds("import IconDots from '~icons/ph/dots-three-circle-bold';")).toEqual([]);
		expect(kinds("import IconRaw from '~icons/ph/gear-bold?raw';")).toEqual([]);
	});

	it('exempts the theme module and generated files', () => {
		expect(isExempt('src/lib/theme/riverstone.ts')).toBe(true);
		expect(isExempt('src/lib/api/schema.d.ts')).toBe(true);
		expect(isExempt('src/lib/ui/Button.svelte')).toBe(false);
	});
});
