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
		labelOf: (v: string) => valueLabel(name, v, 'en'),
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
