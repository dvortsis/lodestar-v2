import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
// @ts-expect-error a plain .mjs script with no type declarations
import {
	CLOSE,
	EXPANSION,
	OPEN,
	pseudoCatalogue,
	pseudoText,
	safeToReplace,
	scratchDir
} from './pseudo-locale.mjs';

const plainLetters = /[A-Za-z]/;

describe('pseudoText', () => {
	it('wraps the message in ⟦ ⟧ and leaves no plain Latin letter', () => {
		const out: string = pseudoText('Search the catalogue');
		expect(out.startsWith(OPEN)).toBe(true);
		expect(out.endsWith(CLOSE)).toBe(true);
		expect(plainLetters.test(out)).toBe(false);
	});

	it('maps every ASCII letter to its own, different, accented letter', () => {
		const letters = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ';
		const out = [...(pseudoText(letters) as string).slice(1, 1 + letters.length)];
		expect(out).toHaveLength(52);
		expect(new Set(out).size).toBe(52);
		for (const [i, c] of out.entries()) expect(c).not.toBe(letters[i]);
	});

	it('keeps {placeholders} exactly, so the app can still fill them', () => {
		const out: string = pseudoText('{count} new of {total} torrents');
		expect(out).toContain('{count}');
		expect(out).toContain('{total}');
		expect(out.match(/\{[^{}]*\}/g)).toEqual(['{count}', '{total}']);
	});

	it('grows a message by about a third, measured without its placeholders', () => {
		const text = 'Save this search and get an alert';
		const out: string = pseudoText(text);
		const body = [...out].length - 2; // without ⟦ ⟧
		expect(body).toBeGreaterThanOrEqual(Math.floor(text.length * (1 + EXPANSION)) - 1);
		expect(body).toBeLessThanOrEqual(Math.ceil(text.length * (1 + EXPANSION)) + 1);
	});

	it('does not break on an empty message or one made only of a placeholder', () => {
		expect(pseudoText('')).toBe(`${OPEN}${CLOSE}`);
		expect(pseudoText('{x}')).toBe(`${OPEN}{x}${CLOSE}`);
	});

	it('is deterministic', () => {
		expect(pseudoText('Same in, same out')).toBe(pseudoText('Same in, same out'));
	});
});

describe('pseudoCatalogue', () => {
	it('keeps the keys and changes only the string messages', () => {
		const out = pseudoCatalogue({ $schema: 'https://example.test/s.json', a: 'One', b: 'Two {n}' });
		expect(Object.keys(out)).toEqual(['$schema', 'a', 'b']);
		expect(out.$schema).toBe('https://example.test/s.json');
		expect(out.a.startsWith(OPEN)).toBe(true);
		expect(out.b).toContain('{n}');
	});
});

describe('scratchDir', () => {
	const repo = resolve(import.meta.dirname, '..', '..');

	it('is outside the repository by default, one folder per port', () => {
		expect(relative(repo, scratchDir({})).startsWith('..')).toBe(true);
		expect(scratchDir({}, 4174)).not.toBe(scratchDir({}, 4175));
	});

	it('refuses a place inside the repository', () => {
		expect(() => scratchDir({ PSEUDO_DIR: './build' })).toThrow(/inside the repository/);
		expect(() => scratchDir({ PSEUDO_DIR: '../docs/pseudo' })).toThrow(/inside the repository/);
	});

	it('refuses a folder that holds the repository, which the build would delete', () => {
		expect(() => scratchDir({ PSEUDO_DIR: resolve(repo, '..') })).toThrow(
			/contains the repository/
		);
		expect(() => scratchDir({ PSEUDO_DIR: '/' })).toThrow(/contains the repository/);
	});
});

describe('safeToReplace', () => {
	it('allows a missing folder, an empty one and one this script made; not another', () => {
		const dir = mkdtempSync(join(tmpdir(), 'pseudo-test-'));
		try {
			expect(safeToReplace(join(dir, 'missing'))).toBe(true);
			expect(safeToReplace(dir)).toBe(true); // empty
			writeFileSync(join(dir, 'notes.txt'), "someone else's file");
			expect(safeToReplace(dir)).toBe(false);
			writeFileSync(join(dir, '.lodestar-pseudo-scratch'), '');
			expect(safeToReplace(dir)).toBe(true);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});
});
