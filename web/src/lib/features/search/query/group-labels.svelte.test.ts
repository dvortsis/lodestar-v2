import { flushSync } from 'svelte';
import { beforeEach, describe, expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { chipClauses, label } from './clauses';
import { CAP, fitsValue, groupText, rememberGroups, resetGroupLabels } from './group-labels.svelte';
import { rowOf } from '../assist/builder';

const group = (...any: string[]): Ast => ({ op: 'tag', key: 'group', any });

beforeEach(() => resetGroupLabels());

describe('the S1-20 check', () => {
	it('passes the value with ASCII letters re-cased, and only that', () => {
		expect(fitsValue('rarbg', 'RARBG')).toBe(true);
		expect(fitsValue('ntb-hd', 'NTb-HD')).toBe(true);
		expect(fitsValue('rarbg', 'rarbg')).toBe(true);
		// Only upper case in the label over the value's lower case, as fit_label: never the reverse.
		expect(fitsValue('RARBG', 'rarbg')).toBe(false);
		expect(fitsValue('Rarbg', 'RARBG')).toBe(true);
		expect(fitsValue('Rarbg', 'rarbg')).toBe(false);
		// Another spelling, length or character never passes.
		expect(fitsValue('rarbg', 'RARBG2')).toBe(false);
		expect(fitsValue('rarbg', 'RARB6')).toBe(false);
		expect(fitsValue('a-b', 'A_B')).toBe(false);
		// Characters 0x20 apart that aren't a letter pair: `@`/backtick, `[`/`{`.
		expect(fitsValue('@x', '`x')).toBe(false);
		expect(fitsValue('[x', '{x')).toBe(false);
		// Non-ASCII letters never re-case (the server's rule): é/É, the Cyrillic в/В.
		expect(fitsValue('café', 'CAFÉ')).toBe(false);
		expect(fitsValue('café', 'CAFé')).toBe(true);
		expect(fitsValue('вг', 'ВГ')).toBe(false);
	});
});

describe('the session map', () => {
	it('keeps a passing label and drops a failing one, which never enters', () => {
		rememberGroups({
			values: [
				{ value: 'rarbg', label: 'RARBG' },
				{ value: 'yts', label: 'YIFY' },
				{ value: 'evo', label: '<b>EVO</b>' },
				{ value: 'plain' }
			]
		});
		expect(groupText('rarbg')).toBe('RARBG');
		expect(groupText('yts')).toBe('yts');
		expect(groupText('evo')).toBe('evo');
		expect(groupText('plain')).toBe('plain');
		expect(groupText('unknown')).toBe('unknown');
	});

	it('keeps at most CAP labels and drops the oldest first', () => {
		const value = (i: number) => `g${i}`;
		rememberGroups({
			values: Array.from({ length: CAP }, (_, i) => ({
				value: value(i),
				label: value(i).toUpperCase()
			}))
		});
		expect(groupText(value(0))).toBe('G0');
		// One more than CAP evicts g0, the oldest, and only it.
		rememberGroups({ values: [{ value: 'new', label: 'NEW' }] });
		expect(groupText(value(0))).toBe(value(0));
		expect(groupText(value(1))).toBe('G1');
		expect(groupText(value(2))).toBe('G2');
		expect(groupText('new')).toBe('NEW');
	});

	it('moves every value a response lists to the newest end, so that response never evicts it', () => {
		const value = (i: number) => `g${i}`;
		rememberGroups({
			values: Array.from({ length: CAP }, (_, i) => ({
				value: value(i),
				label: value(i).toUpperCase()
			}))
		});
		// g0 is the oldest; this response lists it again, with the same label, beside a new value.
		rememberGroups({
			values: [
				{ value: value(0), label: 'G0' },
				{ value: 'new', label: 'NEW' }
			]
		});
		expect(groupText(value(0))).toBe('G0');
		expect(groupText('new')).toBe('NEW');
		expect(groupText(value(1))).toBe(value(1)); // the oldest unlisted one went instead
		// A relabel moves it too: g2 is now the oldest, and it isn't dropped by its own response.
		rememberGroups({
			values: [
				{ value: value(2), label: 'G2' },
				{ value: 'newer', label: 'NEWER' }
			]
		});
		expect(groupText(value(2))).toBe('G2');
		expect(groupText(value(3))).toBe(value(3));
	});

	it('follows the latest response: a value it lists with no label, or a failing one, loses its label', () => {
		rememberGroups({
			values: [
				{ value: 'rarbg', label: 'RARBG' },
				{ value: 'yts', label: 'YTS' }
			]
		});
		// The server leaves `label` out when the winning spelling is the value (facets.rs).
		rememberGroups({ values: [{ value: 'rarbg' }, { value: 'yts', label: 'YIFY' }] });
		expect(groupText('rarbg')).toBe('rarbg');
		expect(groupText('yts')).toBe('yts');
		// An empty or missing facet changes nothing.
		rememberGroups({ values: [{ value: 'evo', label: 'EVO' }] });
		rememberGroups(undefined);
		rememberGroups({ values: [] });
		expect(groupText('evo')).toBe('EVO');
	});
});

describe('the chip re-renders', () => {
	it('a chip held in $derived shows the label once a response brings it', () => {
		const node = group('rarbg');
		const seen: string[] = [];
		const stop = $effect.root(() => {
			const text = $derived(chipClauses(node)[0]!.label.text);
			$effect(() => {
				seen.push(text);
			});
		});
		flushSync();
		rememberGroups({ values: [{ value: 'rarbg', label: 'RARBG' }] });
		flushSync();
		stop();
		expect(seen).toEqual(['Release group: rarbg', 'Release group: RARBG']);
	});
});

describe('the chip and the Builder row', () => {
	it('show the label when known, the value when not, and never change the query', () => {
		const node = group('rarbg', 'yts');
		const before = JSON.stringify(node);
		expect(label(node).text).toBe('Release group: rarbg, yts');
		rememberGroups({ values: [{ value: 'rarbg', label: 'RARBG' }] });
		expect(label(node).text).toBe('Release group: RARBG, yts');
		// The label is a mono part of the value's own length, so the chip doesn't shift.
		expect(label(group('rarbg')).parts).toContainEqual({ mono: 'RARBG' });
		// The AST, and so the query text, URL, copy, share and saved searches, keep the value.
		expect(JSON.stringify(node)).toBe(before);
		expect(chipClauses(node)[0]!.node).toEqual(group('rarbg', 'yts'));
		// The Builder's input is the query text it writes: raw (runner, §2.26).
		expect(rowOf(node)).toEqual({ field: 'group', op: ':', value: 'rarbg,yts' });
	});

	it('only relabels groups: another tag with the same value keeps it', () => {
		rememberGroups({ values: [{ value: 'x264', label: 'X264' }] });
		expect(label({ op: 'tag', key: 'codec', any: ['x264'] } as Ast).text).not.toContain('X264');
	});
});
