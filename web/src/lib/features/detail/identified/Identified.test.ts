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
	it('renders nothing for no answers and a null resolved_via, and never says "from the name"', () => {
		const { container } = show({ alternatives: [], resolved_via: null });
		expect(container.querySelector('section')).toBeNull();
		expect(container.textContent).not.toMatch(/name/i);
	});
	it('says how a file-tree resolution was reached, only when resolved_via is set', () => {
		show({ alternatives: [], resolved_via: 'folder' });
		expect(text('detail-identified-via')).toBe('Resolved via folder');
	});
	it('shows no via line when an answer exists but resolved_via is null', () => {
		show({ alternatives: [answer()], resolved_via: null });
		expect(screen.queryByTestId('detail-identified-via')).toBeNull();
	});
	it('renders stored text as text, with direction controls made visible and isolated', () => {
		const evil = '<img src=x onerror=alert(1)>‮evil';
		const { container } = show({
			alternatives: [
				answer({ canonical_title: evil, alt_titles: [{ language: 'x"><b>', title: evil }] })
			],
			resolved_via: null
		});
		expect(container.querySelector('img')).toBeNull();
		expect(container.querySelector('b')).toBeNull();
		const canon = screen.getByTestId('detail-identified-canonical');
		expect(canon.textContent).toContain('<U+202E>evil');
		expect(canon.textContent).not.toContain('‮');
		expect(canon.querySelector('bdi')).not.toBeNull();
		expect(
			screen.getByTestId('detail-identified-alt').querySelector('bdi')?.hasAttribute('lang')
		).toBe(false);
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
