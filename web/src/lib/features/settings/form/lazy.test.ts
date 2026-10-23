// The parts the engine loads on demand (form.svelte.ts's header): each test starts from a fresh
// module graph, so nothing is loaded yet, and records which chunks load and when.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DRAFT_PREFIX } from './draft-store';
import type { SettingsForm } from './form.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';
import type { FieldSpec } from './types';

const loads: string[] = [];
let failing = new Set<string>();

/** Records each on-demand module as it's evaluated (`preview` comes with `edit` too). */
function spyOnLoads() {
	for (const name of ['edit', 'instant', 'preview']) {
		vi.doMock(`./${name}`, async (original: () => Promise<unknown>) => {
			if (failing.has(name)) throw new Error(`${name} failed to load`);
			loads.push(name);
			return original();
		});
	}
}

const SPECS: FieldSpec[] = [
	spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced', 'aggressive'] }),
	spec('crawler.timeout_secs', { kind: 'integer', minimum: 1, maximum: 120 }),
	spec('transport.utp', { kind: 'boolean' })
];
const PREVIEWED = spec('crawler.max_sockets', { kind: 'integer', auto: 0, preview: true });

let server: ReturnType<typeof fakeServer>;
const forms: SettingsForm[] = [];
// Up to 10 s, for the slowest CI runner; each test gets 20.
const until = (check: () => void) => vi.waitFor(check, { timeout: 10_000 });

async function open(
	over: Partial<Parameters<typeof import('./form.svelte').createSettingsForm>[0]> = {}
) {
	const { createSettingsForm } = await import('./form.svelte');
	const form = createSettingsForm({ id: 'lazy', fields: SPECS, ...over });
	forms.push(form);
	await until(() => expect(form.status).toBe('ready'));
	return form;
}

beforeEach(() => {
	vi.resetModules(); // nothing loaded yet
	loads.length = 0;
	failing = new Set();
	spyOnLoads();
	sessionStorage.clear();
	server = fakeServer([
		values('crawler', 3, { intensity: sv('balanced'), timeout_secs: sv(12), max_sockets: sv(0) }),
		values('transport', 7, { utp: sv(true) })
	]);
	vi.stubGlobal('fetch', server.fetch);
});

afterEach(() => {
	for (const f of forms.splice(0)) f.destroy();
	vi.unstubAllGlobals();
});

/** What has loaded, in name order. */
const loaded = () => [...loads].sort();
const patches = () => server.requests('PATCH', '/settings').map((r) => r.body);

describe('a staged section', { timeout: 20_000 }, () => {
	it('loads none of them for its first view', async () => {
		const form = await open();
		form.field('crawler.intensity');
		// A cold import of its own takes as long as a stray load would: one started earlier is in.
		await import('./copy');
		expect(loaded()).toEqual([]);
	});

	it('loads the edit chunk with the first edit, and saves through it', async () => {
		const form = await open({ drafts: false }); // no draft write to load it instead
		form.set('crawler.intensity', 'low');
		expect(form.dirtyCount).toBe(1); // staged at once, before the chunk arrives
		await until(() => expect(loaded()).toEqual(['edit', 'preview']));
		expect(await form.save()).toEqual({ ok: true, restart: [] });
		expect(patches()).toEqual([{ crawler: { if_revision: 3, intensity: 'low' } }]);
		expect(loaded()).toEqual(['edit', 'preview']); // once
	});

	it('writes the draft once the chunk is in', async () => {
		const form = await open();
		form.set('crawler.timeout_secs', 30);
		await until(() => expect(sessionStorage.getItem(DRAFT_PREFIX + 'lazy')).toContain('30'));
		expect(form.dirtyCount).toBe(1);
	});

	it('validates on blur once the chunk is in', async () => {
		const form = await open();
		form.set('crawler.timeout_secs', 500);
		form.blur('crawler.timeout_secs'); // before the chunk: checked when it arrives
		await until(() =>
			expect(form.field('crawler.timeout_secs').error).toMatch(/between 1 and 120/)
		);
	});

	it('loads it with the values to offer a stored draft', async () => {
		sessionStorage.setItem(
			DRAFT_PREFIX + 'lazy',
			JSON.stringify({
				v: 1,
				at: 1,
				sections: {
					crawler: {
						revision: 3,
						base: { intensity: 'balanced' },
						mine: { intensity: 'low' },
						reset: []
					}
				}
			})
		);
		const form = await open();
		expect(loaded()).toEqual(['edit', 'preview']);
		expect(form.draft).toEqual({ fields: 1, dropped: 0 }); // offered with the first view
		form.restoreDraft();
		expect(form.field('crawler.intensity')).toMatchObject({ value: 'low', dirty: true });
	});

	it('loads it to merge an event that arrives before any edit', async () => {
		const form = await open();
		const revision = server.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
		form.received({ section: 'crawler', revision });
		await until(() => expect(form.field('crawler.intensity').value).toBe('aggressive'));
		expect(loaded()).toEqual(['edit', 'preview']);
	});

	it('loads previews on their own, during the first one’s debounce', async () => {
		const form = await open({ fields: [...SPECS, PREVIEWED] });
		await until(() => expect(server.requests('POST', '/settings/preview')).toHaveLength(1));
		expect(loaded()).toEqual(['preview']);
		expect(form.dirtyCount).toBe(0);
	});

	it('reports a chunk that fails to load as a network failure, and tries again', async () => {
		failing = new Set(['edit']);
		const form = await open();
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'network' });
		expect(patches()).toEqual([]);
		failing = new Set();
		expect(await form.save()).toEqual({ ok: true, restart: [] });
		expect(loaded()).toEqual(['edit', 'preview']);
	});
});

describe('an instant section', { timeout: 20_000 }, () => {
	it('loads instant mode with the first change, and sends it', async () => {
		const form = await open({ mode: 'instant' });
		expect(loaded()).toEqual([]);
		form.set('transport.utp', false);
		await until(() => expect(patches()).toEqual([{ transport: { if_revision: 7, utp: false } }]));
		expect(loaded()).toEqual(['edit', 'instant', 'preview']);
		expect(form.field('transport.utp')).toMatchObject({ value: false, dirty: false });
		expect(await form.undo('transport.utp')).toMatchObject({ ok: true });
	});

	it('keeps changes made while it loads in order', async () => {
		const form = await open({ mode: 'instant' });
		form.set('transport.utp', false);
		form.set('crawler.intensity', 'low');
		await until(() => expect(patches()).toHaveLength(2));
		expect(patches()).toEqual([
			{ transport: { if_revision: 7, utp: false } },
			{ crawler: { if_revision: 3, intensity: 'low' } }
		]);
	});

	it('puts the value back with an error when it can’t load', async () => {
		failing = new Set(['instant']);
		const form = await open({ mode: 'instant' });
		form.set('transport.utp', false);
		await until(() =>
			expect(form.field('transport.utp')).toMatchObject({ value: true, error: expect.any(String) })
		);
		expect(patches()).toEqual([]);
	});
});
