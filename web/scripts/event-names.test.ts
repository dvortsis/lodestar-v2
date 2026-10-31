// @vitest-environment node
// The checked-in event-names.ts is what the generator writes for the server's current catalogue,
// so a new or removed server event changes the file (and its messages) in the same PR.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
// @ts-expect-error: a plain .mjs script with no type declarations
import { CATALOG, OUTPUT, render } from './event-names.mjs';

describe('event-names generator', () => {
	it('matches the checked-in file', async () => {
		const text = await (render as (g: string) => Promise<string>)(readFileSync(CATALOG, 'utf8'));
		expect(text).toBe(readFileSync(OUTPUT, 'utf8'));
	});
});
