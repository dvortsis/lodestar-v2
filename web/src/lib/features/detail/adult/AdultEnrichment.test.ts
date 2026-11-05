import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import type { components } from '$lib/api/schema';
import { searchHit, torrentDetail } from '../fixtures';
import AdultEnrichment from './AdultEnrichment.svelte';
import { safeLink } from './link';

type E = components['schemas']['AdultEnrichment'];
vi.mock('../visibility', () => ({ watchDetailVisibility: vi.fn() }));
// No scrolling in jsdom: the Contents and Parsed chunks stay unloaded, so nothing imports after teardown.
vi.mock('../visible', () => ({ whenVisible: () => ({}) }));
vi.mock('$app/paths', () => ({
	resolve: (route: string, params?: Record<string, string>) =>
		route.replace('[hash]', params?.hash ?? '')
}));
const T = { timeout: 10_000 };
const section = (over: Partial<E> = {}): E => ({
	link: null,
	performers: ['Ada Example', 'Bo Sample'],
	performers_truncated: false,
	released: '2021-03-04',
	scene_title: 'A Scene',
	source: { id: 7, name: 'ThePornDB', plugin: 'theporndb' },
	studio: 'Example Studio',
	...over
});
const show = (adult?: E) =>
	render(AdultEnrichment, { detail: torrentDetail(adult ? { adult_enrichment: adult } : {}) });
const text = (id: string) => screen.getByTestId(id).textContent!.replace(/\s+/g, ' ').trim();

describe('AdultEnrichment', () => {
	it('renders every field and names the source', () => {
		show(section());
		expect(text('detail-adult-studio')).toBe('Studio Example Studio');
		expect(text('detail-adult-released')).toBe('Released 2021-03-04');
		expect(text('detail-adult-scene')).toBe('Scene A Scene');
		const names = [...screen.getByTestId('detail-adult-performers').querySelectorAll('li')];
		expect(names.map((n) => n.textContent)).toEqual(['Ada Example', 'Bo Sample']);
		expect(text('detail-adult-source')).toBe('Matched by ThePornDB');
		expect(screen.queryByTestId('detail-adult-performers-more')).toBeNull();
	});
	it('omits the rows the service left empty', () => {
		show(section({ studio: null, released: null, scene_title: null, performers: [] }));
		for (const k of ['studio', 'released', 'scene', 'performers'])
			expect(screen.queryByTestId(`detail-adult-${k}`)).toBeNull();
		expect(screen.getByTestId('detail-adult-source')).toBeTruthy();
	});
	it('notes truncation', () => {
		show(section({ performers_truncated: true }));
		expect(screen.getByTestId('detail-adult-performers-more')).toBeTruthy();
	});
	it('shows script-like text as text', () => {
		const evil = '<img src=x onerror=alert(1)><script>alert(2)</script>';
		const { container } = show(
			section({
				studio: evil,
				scene_title: evil,
				performers: [evil],
				source: { id: 1, name: evil, plugin: 'x' }
			})
		);
		expect(container.querySelector('img, script')).toBeNull();
		expect(text('detail-adult-studio')).toContain(evil);
		expect(text('detail-adult-source')).toContain(evil);
	});
	it('has no link when it is null', () => {
		show(section({ link: null }));
		expect(screen.queryByTestId('detail-adult-link')).toBeNull();
	});
	it.each(['http://example.com/s', 'javascript:alert(1)', 'data:text/html,x', '//example.com', ''])(
		'has no link for %j',
		(link) => {
			show(section({ link }));
			expect(screen.queryByTestId('detail-adult-link')).toBeNull();
		}
	);
	it('opens an https link in a new tab, safely', () => {
		show(section({ link: 'https://theporndb.net/scenes/1' }));
		const a = screen.getByTestId('detail-adult-link');
		expect(a.getAttribute('href')).toBe('https://theporndb.net/scenes/1');
		expect(a.getAttribute('target')).toBe('_blank');
		expect(a.getAttribute('rel')).toBe('noopener noreferrer');
	});
});

describe('AdultEnrichment (absent)', () => {
	it('renders nothing when the detail has no section', () => {
		const { container } = show();
		expect(container.textContent!.trim()).toBe('');
		expect(screen.queryByTestId('detail-adult-enrichment')).toBeNull();
	});
});

describe('safeLink', () => {
	it('accepts https only', () => {
		expect(safeLink('https://a.example/x')).toBe('https://a.example/x');
		expect(safeLink('HTTPS://a.example/x')).toBe('https://a.example/x');
		expect(safeLink('http://a.example/x')).toBeNull();
		expect(safeLink('https:')).toBeNull();
		expect(safeLink('not a url')).toBeNull();
		expect(safeLink(undefined)).toBeNull();
	});
});

describe('DetailView mount', () => {
	// A fresh module graph with the section's chunk replaced, to see whether the view asks for it.
	async function mount(adult?: E) {
		vi.resetModules();
		const loaded = vi.fn();
		vi.doMock('./AdultEnrichment.svelte', async () => {
			loaded();
			return await vi.importActual<object>('./AdultEnrichment.svelte');
		});
		// Svelte's runtime is reloaded too, so render with the testing library from the same graph.
		const { render: renderFresh, screen: screenFresh } = await import('@testing-library/svelte');
		const { default: View } = await import('../DetailView.svelte');
		const { DetailModel } = await import('../model.svelte');
		const model = new DetailModel();
		model.detail = torrentDetail(adult ? { adult_enrichment: adult } : {});
		model.hit = searchHit();
		renderFresh(View, { model, mode: 'drawer' });
		return { loaded, screen: screenFresh };
	}
	afterEach(() => vi.doUnmock('./AdultEnrichment.svelte'));

	it('shows nothing, and never loads the chunk, when the field is absent', async () => {
		const { loaded, screen } = await mount();
		expect(screen.getByTestId('detail-view')).toBeTruthy();
		// The view's own lazy imports are queued behind this one; give them the chance to run.
		for (let i = 0; i < 50; i++) await Promise.resolve();
		expect(screen.queryByTestId('detail-adult-enrichment')).toBeNull();
		expect(loaded).not.toHaveBeenCalled();
	});
	it('loads and shows the section when the API sent it', async () => {
		const { loaded, screen } = await mount(section());
		expect(await screen.findByTestId('detail-adult-enrichment', {}, T)).toBeTruthy();
		expect(loaded).toHaveBeenCalledOnce();
		expect(text('detail-adult-source')).toBe('Matched by ThePornDB');
	});
});
