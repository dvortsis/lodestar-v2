import { describe, expect, it } from 'vitest';
import {
	PAGE_BODY_FULL,
	PAGE_BODY_MANUSCRIPT,
	PAGE_BODY_WIDE,
	PAGE_FRAME,
	PAGE_H1,
	PAGE_HEADER
} from './classes';

const all = {
	PAGE_FRAME,
	PAGE_BODY_FULL,
	PAGE_BODY_WIDE,
	PAGE_BODY_MANUSCRIPT,
	PAGE_HEADER,
	PAGE_H1
};
const tokens = (s: string) => s.split(/\s+/);

describe('page frame classes', () => {
	it('has the shared gutters, 16 px and 24 px from large, and is never centred', () => {
		expect(tokens(PAGE_FRAME)).toEqual(expect.arrayContaining(['px-4', 'large:px-6']));
		for (const value of Object.values(all)) expect(tokens(value)).not.toContain('mx-auto');
	});

	it('caps the content box only by width, and never the gutters', () => {
		expect(PAGE_BODY_FULL).not.toContain('max-w');
		expect(tokens(PAGE_BODY_WIDE)).toContain('max-w-7xl');
		expect(tokens(PAGE_BODY_MANUSCRIPT)).toContain('max-w-(--manuscript-width)');
		expect(PAGE_FRAME).not.toContain('max-w');
	});

	it('is the display h1 with the shared focus ring', () => {
		expect(tokens(PAGE_H1)).toEqual(
			expect.arrayContaining(['text-display', 'font-semibold', 'focus-visible:outline-primary'])
		);
	});

	it('uses no hex and no arbitrary values (the spacing steps are checked by the bans script)', () => {
		for (const value of Object.values(all))
			for (const t of tokens(value)) expect(t).not.toMatch(/#|\[/);
	});
});
