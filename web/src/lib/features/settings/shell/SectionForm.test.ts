import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import SectionForm from './SectionForm.svelte';
import { screens } from './screens';
import { fakeServer } from '../form/testing/fake-server';
import { spec, sv, values } from '../form/testing/values';
import type { FieldSpec } from '../form/types';

const navigation = vi.hoisted(() => ({ guards: [] as ((nav: unknown) => void)[] }));
const state = vi.hoisted(() => ({ page: { url: new URL('http://localhost/settings/crawler') } }));
vi.mock('$app/state', () => state);
vi.mock('$app/navigation', () => ({
	beforeNavigate: (guard: (nav: unknown) => void) => navigation.guards.push(guard),
	goto: vi.fn()
}));
vi.mock('$lib/state/events.svelte', () => ({ events: { on: () => () => {} } }));

vi.setConfig({ testTimeout: 15_000 });

let server: ReturnType<typeof fakeServer>;
const fields: FieldSpec[] = [
	spec('crawler.paused', { kind: 'boolean', group: 'crawler' }),
	spec('transport.utp', { kind: 'boolean', group: 'transport', advanced: true })
];
const mount = (specs = fields) =>
	render(SectionForm, { screen: screens.find((s) => s.id === 'crawler')!, fields: specs });
const loaded = () =>
	waitFor(() => expect(screen.getByRole('switch', { name: 'Pause crawling' })).toBeTruthy(), {
		timeout: 10_000
	});

beforeEach(() => {
	navigation.guards = [];
	state.page.url = new URL('http://localhost/settings/crawler');
	sessionStorage.clear();
	localStorage.clear();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	Element.prototype.scrollIntoView = vi.fn();
	server = fakeServer([
		values('crawler', 4, { paused: sv(false) }),
		values('transport', 7, { utp: sv(false, 'db') })
	]);
	vi.stubGlobal('fetch', server.fetch);
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

it('saves two API sections atomically with their own revisions', async () => {
	mount();
	await loaded();
	await fireEvent.click(screen.getByRole('switch', { name: 'Pause crawling' }));
	const details = document.querySelector('details')!;
	details.open = true;
	await fireEvent(details, new Event('toggle'));
	await fireEvent.click(screen.getByRole('switch', { name: 'uTP (not active yet)' }));
	// The save bar comes with the edit chunk, which the first edit loads.
	await fireEvent.click(
		await screen.findByRole('button', { name: 'Save changes' }, { timeout: 10_000 })
	);
	await waitFor(() => expect(server.requests('PATCH', '/settings')).toHaveLength(1), {
		timeout: 10_000
	});
	expect(server.requests('PATCH', '/settings')[0]!.body).toEqual({
		crawler: { paused: true, if_revision: 4 },
		transport: { utp: true, if_revision: 7 }
	});
});

it('changed-only preserves staged edits and hides default values', async () => {
	mount();
	await loaded();
	await fireEvent.click(screen.getByRole('switch', { name: 'Show changed only' }));
	expect(screen.queryByRole('switch', { name: 'Pause crawling' })).toBeNull();
	await fireEvent.click(screen.getByRole('switch', { name: 'Show changed only' }));
	await fireEvent.click(screen.getByRole('switch', { name: 'Pause crawling' }));
	await fireEvent.click(screen.getByRole('switch', { name: 'Show changed only' }));
	expect(screen.getByRole('switch', { name: 'Pause crawling' })).toBeTruthy();
});

it('restores Advanced preference and tolerates malformed stored data', async () => {
	localStorage.setItem('lodestar.settings.advanced.crawler', '{');
	const first = mount();
	await loaded();
	const details = document.querySelector('details')!;
	expect(details.open).toBe(false);
	details.open = true;
	await fireEvent(details, new Event('toggle'));
	expect(JSON.parse(localStorage.getItem('lodestar.settings.advanced.crawler')!)).toEqual({
		transport: true
	});
	first.unmount();
	mount();
	await loaded();
	expect(document.querySelector('details')!.open).toBe(true);
});

it('reveals and focuses an Advanced deep link after loading', async () => {
	state.page.url = new URL('http://localhost/settings/crawler#transport.utp');
	mount();
	await waitFor(
		() =>
			expect(document.activeElement).toBe(
				screen.getByRole('switch', { name: 'uTP (not active yet)' })
			),
		{ timeout: 10_000 }
	);
	expect(document.querySelector('details')!.open).toBe(true);
});

it('waits for the form to load before it focuses a #field link', async () => {
	state.page.url = new URL('http://localhost/settings/crawler#crawler.paused');
	let release!: () => void;
	const gate = new Promise<void>((resolve) => (release = resolve));
	const fetchNow = server.fetch;
	let asked = 0;
	vi.stubGlobal('fetch', async (...args: Parameters<typeof fetch>) => {
		asked++;
		await gate;
		return fetchNow(args[0] as string, args[1]);
	});
	mount();
	await waitFor(() => expect(asked).toBeGreaterThan(0), { timeout: 10_000 });
	expect(screen.queryByRole('switch', { name: 'Pause crawling' })).toBeNull();
	expect(document.activeElement).toBe(document.body);
	release();
	await waitFor(
		() =>
			expect(document.activeElement).toBe(screen.getByRole('switch', { name: 'Pause crawling' })),
		{ timeout: 10_000 }
	);
});

it('waits for a lazy choice and focuses its selected option instead of the menu', async () => {
	server.sections.get('crawler')!.fields.intensity = sv('balanced');
	state.page.url = new URL('http://localhost/settings/crawler#crawler.intensity');
	mount([
		...fields,
		spec('crawler.intensity', {
			kind: 'enum',
			enum: ['low', 'balanced', 'aggressive'],
			group: 'crawler'
		})
	]);
	await waitFor(
		() => expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'Balanced' })),
		{ timeout: 10_000 }
	);
});

it('enforces screen read-only fields even when server values are writable', async () => {
	mount(fields.map((field) => ({ ...field, readOnly: true })));
	await loaded();
	expect(
		(screen.getByRole('switch', { name: 'Pause crawling' }) as HTMLInputElement).disabled
	).toBe(true);
	expect(screen.queryByRole('button', { name: 'Save changes' })).toBeNull();
	expect(server.requests('PATCH', '/settings')).toHaveLength(0);
});

it('ignores a late section response after the screen unmounts', async () => {
	const release = server.hold('GET /settings/crawler');
	const view = mount();
	await waitFor(() => expect(server.requests('GET', '/settings/crawler')).toHaveLength(1), {
		timeout: 10_000
	});
	view.unmount();
	release();
	expect(screen.queryByRole('switch', { name: 'Pause crawling' })).toBeNull();
	expect(server.requests('PATCH', '/settings')).toHaveLength(0);
});
