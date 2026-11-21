// The facet "Find" box (X-labels review): it matches what's shown and the code a query types, so
// "French" and `fr`, "H.264" and `h264` both find their entry, in any case.
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { valueLabel } from '$lib/format/label-release';
import TermGroup from './TermGroup.svelte';
import { term } from './fixtures';

const group = (name: string, values: [string, number][]) =>
	render(TermGroup, {
		name,
		title: name,
		facet: term(values),
		labelOf: (v: string, label?: string | null) => valueLabel(name, v, 'en', label),
		more: true,
		onmore: vi.fn(),
		onvalue: vi.fn(),
		onclear: vi.fn()
	});
const find = async (name: string, text: string) =>
	fireEvent.input(screen.getByTestId(`facet-${name}-find`), { target: { value: text } });
const listed = (name: string, values: string[]) =>
	values.filter((v) => screen.queryByTestId(`facet-${name}-${v}-count`));

describe('Find in a long facet list', () => {
	it('matches a language by its name or its code', async () => {
		group('lang', [
			['de', 4],
			['fr', 3],
			['zh', 2]
		]);
		await find('lang', 'German');
		expect(listed('lang', ['de', 'fr', 'zh'])).toEqual(['de']);
		await find('lang', 'zh');
		expect(listed('lang', ['de', 'fr', 'zh'])).toEqual(['zh']);
		await find('lang', 'FR');
		expect(listed('lang', ['de', 'fr', 'zh'])).toEqual(['fr']);
	});
	it('matches a codec by its name or its code', async () => {
		group('audio', [
			['eac3', 5],
			['dts-hd-ma', 2],
			['aac', 9]
		]);
		await find('audio', 'dolby');
		expect(listed('audio', ['eac3', 'dts-hd-ma', 'aac'])).toEqual(['eac3']);
		await find('audio', 'dts-hd-ma');
		expect(listed('audio', ['eac3', 'dts-hd-ma', 'aac'])).toEqual(['dts-hd-ma']);
		await find('audio', 'EAC3');
		expect(listed('audio', ['eac3', 'dts-hd-ma', 'aac'])).toEqual(['eac3']);
	});
	it('lists everything with an empty box, and nothing for no match', async () => {
		group('codec', [
			['h264', 5],
			['h265', 3]
		]);
		await find('codec', '');
		expect(listed('codec', ['h264', 'h265'])).toEqual(['h264', 'h265']);
		await find('codec', 'vp9');
		expect(listed('codec', ['h264', 'h265'])).toEqual([]);
	});
});

// X-facet-group-case: the index keeps release groups in lower case; the server sends the spelling
// the torrents' names use as `label`. It's shown, and found by "Find"; a click still sends the value.
describe('A release group shows the spelling the server sends', () => {
	const groups = () => {
		const facet = term([
			['rarbg', 5],
			['ntb', 3]
		]);
		facet.values[0].label = 'RARBG';
		const onvalue = vi.fn();
		render(TermGroup, {
			name: 'group',
			title: 'group',
			facet,
			labelOf: (v: string, label?: string | null) => valueLabel('group', v, 'en', label),
			more: true,
			onmore: vi.fn(),
			onvalue,
			onclear: vi.fn()
		});
		return onvalue;
	};
	it('shows the label, or the value when there is none', () => {
		groups();
		expect(screen.getByText('RARBG')).toBeTruthy();
		expect(screen.queryByText('rarbg')).toBeNull();
		expect(screen.getByText('ntb')).toBeTruthy();
	});
	it('finds it by the label or the value, and a click sends the value', async () => {
		const onvalue = groups();
		await find('group', 'RARBG');
		expect(listed('group', ['rarbg', 'ntb'])).toEqual(['rarbg']);
		await find('group', 'rarbg');
		expect(listed('group', ['rarbg', 'ntb'])).toEqual(['rarbg']);
		await find('group', '');
		await fireEvent.click(screen.getByTestId('facet-group-rarbg'));
		expect(onvalue).toHaveBeenCalledWith('rarbg', 'include');
	});
	it('falls back to the value with no label', () => {
		expect(valueLabel('group', 'rarbg', 'en')).toBe('rarbg');
		expect(valueLabel('group', 'rarbg', 'en', null)).toBe('rarbg');
		expect(valueLabel('group', 'rarbg', 'en', 'RARBG')).toBe('RARBG');
		// Another field ignores a label.
		expect(valueLabel('codec', 'h264', 'en', 'X')).toBe('H.264');
	});
});
