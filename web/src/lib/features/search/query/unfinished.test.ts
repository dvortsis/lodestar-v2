import { describe, expect, it } from 'vitest';
import { isUnfinished } from './unfinished';

describe('isUnfinished', () => {
	const error = { from: 4, to: 8 };
	it('is true while the caret is in the token and suggestions are open', () => {
		expect(isUnfinished(error, 4)).toBe(true);
		expect(isUnfinished(error, 6)).toBe(true);
		expect(isUnfinished(error, 8)).toBe(true);
	});
	it('is false with the suggestions closed (no caret), e.g. after Enter', () => {
		expect(isUnfinished(error, null)).toBe(false);
	});
	it('is false when the caret is in another token', () => {
		expect(isUnfinished(error, 2)).toBe(false);
		expect(isUnfinished(error, 12)).toBe(false);
	});
	it('is false with no error', () => {
		expect(isUnfinished(null, 3)).toBe(false);
	});
});
