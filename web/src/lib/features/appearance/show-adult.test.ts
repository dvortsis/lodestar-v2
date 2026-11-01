import { describe, expect, it } from 'vitest';
import { SHOW_ADULT_ENV as flows } from '../filters/flows/adult.svelte';
import { SHOW_ADULT_ENV } from './show-adult';

describe('SHOW_ADULT_ENV', () => {
	it("is the flows' variable name", () => expect(SHOW_ADULT_ENV).toBe(flows));
});
