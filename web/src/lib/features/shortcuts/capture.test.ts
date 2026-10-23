import { describe, expect, it } from 'vitest';
import { capture, clash, effective, swap, withKey } from './capture';
import { keyNameFor } from './keyname';

const press = (key: string, init: Partial<KeyboardEvent> = {}) =>
	capture({ key, ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, ...init });

describe('capture', () => {
	it('accepts one visible character, letters in lower case', () => {
		expect(press('n')).toEqual({ key: 'n' });
		expect(press('/')).toEqual({ key: '/' });
		expect(press('?', { shiftKey: true })).toEqual({ key: '?' });
		expect(press('ü')).toEqual({ key: 'ü' });
		expect(press('J')).toEqual({ key: 'j' }); // Caps Lock, no Shift
		expect(press('漢')).toEqual({ key: '漢' });
	});

	it('refuses Shift plus a letter, naming why', () => {
		expect(press('J', { shiftKey: true })).toEqual({ refuse: 'shift' });
	});

	it('refuses Ctrl, Alt and Meta combos', () => {
		expect(press('k', { ctrlKey: true })).toEqual({ refuse: 'modifier' });
		expect(press('k', { altKey: true })).toEqual({ refuse: 'modifier' });
		expect(press('k', { metaKey: true })).toEqual({ refuse: 'modifier' });
	});

	it('refuses what check_shortcuts refuses: space, control, zero-width and combining characters', () => {
		for (const key of [' ', '\t', '\u0007', '​', '́', '‮', '﻿', 'Enter', 'ArrowUp', 'F5', 'ab'])
			expect(press(key), JSON.stringify(key)).toEqual({ refuse: 'blank' });
	});

	it('refuses + (it separates the parts of a combo)', () => {
		expect(press('+')).toEqual({ refuse: 'plus' });
	});

	it('Escape and Tab cancel; a bare modifier press is ignored', () => {
		expect(press('Escape')).toEqual({ cancel: true });
		expect(press('Tab')).toEqual({ cancel: true });
		expect(press('Shift', { shiftKey: true })).toEqual({ ignore: true });
		expect(press('Control', { ctrlKey: true })).toEqual({ ignore: true });
	});
});

describe('conflicts (the cases in preferences.rs)', () => {
	it('a key another action has by default clashes, a swap does not', () => {
		expect(clash({}, 'results.next', 'k')).toBe('results.previous');
		expect(clash({}, 'results.next', 'j')).toBeUndefined();
		const swapped = swap({}, 'results.next', 'results.previous', 'k');
		expect(swapped).toEqual({ 'results.next': 'k', 'results.previous': 'j' });
		for (const id of Object.keys(swapped) as 'results.next'[]) {
			expect(clash(swapped, id, effective(swapped, id))).toBeUndefined();
		}
	});

	it('a remap frees its old key and takes the new one', () => {
		const map = { 'results.next': 'n' };
		expect(clash(map, 'results.previous', 'j')).toBeUndefined();
		expect(clash(map, 'results.previous', 'n')).toBe('results.next');
		expect(clash({ 'search.focus': 'f' }, 'detail.send', '/')).toBeUndefined();
	});

	it('a key equal to the default drops the remap', () => {
		expect(withKey({ 'results.next': 'n' }, 'results.next', 'j')).toEqual({});
	});
});

describe('key names', () => {
	it('speaks symbols', () => {
		expect(keyNameFor('/')).toBe('Slash');
		expect(keyNameFor('?')).toBe('Question mark');
		expect(keyNameFor('mod+/', false)).toBe('Ctrl+Slash');
		expect(keyNameFor('mod+k', false)).toBe('Ctrl+K');
		expect(keyNameFor('j')).toBe('J');
	});
});
