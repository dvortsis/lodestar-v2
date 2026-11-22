import { mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
	CLOSE,
	EXPANSION,
	OPEN,
	newScratchDir,
	plainEnglish,
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

	it('uses one folder per user and port outside the repository by default', () => {
		const a = newScratchDir({}, 4174);
		expect(relative(repo, a).startsWith('..')).toBe(true);
		expect(a).toBe(newScratchDir({}, 4174));
		expect(a).not.toBe(newScratchDir({}, 4175));
	});

	it('refuses a place inside the repository, a name starting with ".." included', () => {
		expect(() => scratchDir({ PSEUDO_DIR: './build' })).toThrow(/inside the repository/);
		expect(() => scratchDir({ PSEUDO_DIR: '../docs/pseudo' })).toThrow(/inside the repository/);
		expect(() => scratchDir({ PSEUDO_DIR: join(repo, '..pseudo') })).toThrow(
			/inside the repository/
		);
	});

	it('refuses a folder that holds the repository, which the build would delete', () => {
		expect(() => scratchDir({ PSEUDO_DIR: resolve(repo, '..') })).toThrow(
			/contains the repository/
		);
		expect(() => scratchDir({ PSEUDO_DIR: '/' })).toThrow(/contains the repository/);
	});

	it('judges real paths: a link into the repository is refused', () => {
		const dir = mkdtempSync(join(tmpdir(), 'pseudo-link-'));
		try {
			symlinkSync(join(repo, 'docs'), join(dir, 'into-repo'));
			expect(() => scratchDir({ PSEUDO_DIR: join(dir, 'into-repo', 'x') })).toThrow(
				/inside the repository/
			);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});

	it('accepts a new folder in the temp folder', () => {
		expect(scratchDir({ PSEUDO_DIR: join(tmpdir(), 'lodestar-pseudo-test-new') })).toContain(
			'lodestar-pseudo-test-new'
		);
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

describe('plainEnglish', () => {
	it('catches plain English and lets accented messages through', () => {
		expect(plainEnglish('Search results')).toBe(true);
		expect(plainEnglish(pseudoText('Search results'))).toBe(false);
	});

	it('catches a bare "Show" button, though "Show" is a fixture title on the torrent page', () => {
		expect(plainEnglish('Show', { route: 'torrent', tag: 'h1' })).toBe(false);
		expect(plainEnglish('Show', { route: 'torrent', tag: 'button' })).toBe(true);
		expect(plainEnglish('Show', { route: 'alerts', tag: 'h1' })).toBe(true);
		expect(plainEnglish('Title 2', { testid: 'discover-row-abc123-title' })).toBe(false);
		expect(plainEnglish('Title 2', { tag: 'label' })).toBe(true);
	});

	it('catches a hard-coded "Today" or "7 days" chip, but not Intl times', () => {
		expect(plainEnglish('Today', { tag: 'button' })).toBe(true);
		expect(plainEnglish('7 days', { tag: 'button' })).toBe(true);
		expect(plainEnglish('3 days ago', { tag: 'time' })).toBe(false);
		expect(plainEnglish('3 days ago', { tag: 'span', time: true })).toBe(false);
		// A time put into a catalogue sentence is inside its ⟦ ⟧.
		expect(plainEnglish(`${OPEN}Šééñ 3 days ago ŀö${CLOSE}`)).toBe(false);
		expect(plainEnglish(`${OPEN}Šééñ ŀö${CLOSE} 3 days ago`, { tag: 'span' })).toBe(true);
	});

	it('allows an absolute Intl date as a whole text, not inside other words', () => {
		expect(plainEnglish('Nov 14, 2023, 22:13', { tag: 'span' })).toBe(false);
		expect(plainEnglish('· Nov 16, 2023, 02:00', { tag: 'span' })).toBe(false);
		expect(plainEnglish('Wed, Oct 21, 2026, 12:00 UTC', { tag: 'td' })).toBe(false);
		expect(plainEnglish(`${OPEN}Ĺöŕéɱ${CLOSE}, Thu, Oct 22, 2026, 06:00 UTC`)).toBe(false);
		expect(plainEnglish('Updated Nov 14, 2023', { tag: 'span' })).toBe(true);
		expect(plainEnglish('Updated Nov 14, 2023, 22:13', { tag: 'span' })).toBe(true);
	});

	it('catches a plain letter that touches an end mark ("⟦…⟧s")', () => {
		expect(plainEnglish(`${OPEN}Ĺöŕéɱ${CLOSE}s`)).toBe(true);
		expect(plainEnglish(`s${OPEN}Ĺöŕéɱ${CLOSE}`)).toBe(true);
		expect(plainEnglish(`${OPEN}Ĺöŕéɱ${CLOSE} 3`)).toBe(false);
	});

	it('does not carry regex state between calls', () => {
		for (let i = 0; i < 4; i++) expect(plainEnglish('Lodestar', {})).toBe(false);
	});
});
