import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import type { components } from '$lib/api/schema';
import { torrentDetail } from '../fixtures';
import Identified from './Identified.svelte';

type I = components['schemas']['IdentifiedAs'];
type A = components['schemas']['Identification'];

const answer = (over: Partial<A> = {}): A => ({
	alt_titles: [{ language: 'ja', title: 'ショーシャンクの空に' }],
	canonical_title: 'The Shawshank Redemption',
	chosen: true,
	confidence: 92,
	genres: ['drama'],
	ids: [{ scheme: 'imdb', value: 'tt0111161' }],
	source: { id: 7, name: 'Wikidata', plugin: 'wikidata' },
	year: 1994,
	...over
});
const show = (identified?: I) =>
	render(Identified, { detail: torrentDetail(identified ? { identified_as: identified } : {}) });
const text = (id: string) => screen.getByTestId(id).textContent!.replace(/\s+/g, ' ').trim();

describe('Identified', () => {
	it('renders each answer with its source, confidence and facts', () => {
		show({ alternatives: [answer()], resolved_via: null });
		expect(screen.getByRole('heading').textContent).toBe('Identified as');
		expect(text('detail-identified-answer')).toContain('Identified by Wikidata');
		expect(text('detail-identified-answer')).toContain('92% sure');
		expect(text('detail-identified-chosen')).toBe('Used as the title');
		expect(text('detail-identified-canonical')).toBe('The Shawshank Redemption');
		expect(text('detail-identified-ids')).toBe('imdb: tt0111161');
		expect(text('detail-identified-genres')).toBe('drama');
		expect(screen.getByTestId('detail-identified-alt').querySelector('bdi')?.lang).toBe('ja');
	});
	it('keeps two disagreeing plugins side by side, in the order sent', () => {
		show({
			alternatives: [
				answer(),
				answer({
					chosen: false,
					canonical_title: 'Other',
					source: { id: 8, name: 'TMDB', plugin: 'tmdb' }
				})
			],
			resolved_via: null
		});
		const blocks = screen.getAllByTestId('detail-identified-answer');
		expect(blocks).toHaveLength(2);
		expect(blocks[1]!.textContent).toContain('Identified by TMDB');
		expect(screen.getAllByTestId('detail-identified-chosen')).toHaveLength(1);
	});
	it('renders nothing when the API left identified_as out', () => {
		const { container } = show();
		expect(container.querySelector('section')).toBeNull();
	});
	it('renders nothing for no answers, whatever resolved_via says', () => {
		for (const via of [null, 'folder']) {
			const { container } = show({ alternatives: [], resolved_via: via });
			expect(container.querySelector('section')).toBeNull();
		}
	});
	it('never presents a null resolved_via as "from the name"', () => {
		const { container } = show({ alternatives: [answer()], resolved_via: null });
		expect(container.textContent).not.toMatch(/from.{0,3}name|resolved via/i);
	});
	it('formats the confidence for the locale', () => {
		show({ alternatives: [answer({ confidence: 92 })], resolved_via: null });
		expect(text('detail-identified-confidence')).toBe('92% sure');
		expect(text('detail-identified-year')).toBe('1994');
	});
	it('puts the heading at the level it is given', () => {
		render(Identified, {
			detail: torrentDetail({ identified_as: { alternatives: [answer()], resolved_via: null } }),
			level: 2
		});
		expect(screen.getByRole('heading', { level: 2 })).toBeTruthy();
	});
	it('renders stored text as text, with direction controls made visible and isolated', () => {
		const evil = '<img src=x onerror=alert(1)>\u202Eevil\u2066x';
		const { container } = show({
			alternatives: [
				answer({
					canonical_title: evil,
					genres: [evil],
					ids: [{ scheme: 'imdb', value: evil }],
					source: { id: 7, name: evil, plugin: 'wikidata' },
					alt_titles: [
						{ language: 'x"><b>', title: evil },
						{ language: 'und', title: 'u' }
					]
				})
			],
			resolved_via: null
		});
		expect(container.querySelector('img')).toBeNull();
		expect(container.querySelector('b')).toBeNull();
		for (const id of [
			'detail-identified-canonical',
			'detail-identified-genres',
			'detail-identified-ids',
			'detail-identified-source',
			'detail-identified-alt'
		]) {
			const el = screen.getByTestId(id);
			expect(el.textContent).toContain('<U+202E>evil<U+2066>x');
			expect(el.textContent).not.toMatch(/[\u202A-\u202E\u2066-\u2069]/);
			expect(el.tagName === 'BDI' || el.querySelector('bdi')).toBeTruthy();
		}
		const alts = screen.getByTestId('detail-identified-alt').querySelectorAll('bdi');
		expect(alts[0]!.hasAttribute('lang')).toBe(false);
		expect(alts[1]!.hasAttribute('lang')).toBe(false);
	});
	it('omits rows with nothing to say', () => {
		show({
			alternatives: [
				answer({ canonical_title: null, year: null, genres: [], ids: [], alt_titles: [] })
			],
			resolved_via: null
		});
		expect(screen.queryByTestId('detail-identified-canonical')).toBeNull();
		expect(screen.queryByTestId('detail-identified-ids')).toBeNull();
	});
});
