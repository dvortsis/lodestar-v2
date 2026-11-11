// Every event the server catalogues has a plain name and a line of help, in both languages; an
// event the client doesn't know falls back to its id. The list is the server's own golden catalogue,
// so a new server event fails here until it's labelled.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';
import { waitFor } from '@testing-library/svelte';
import en from './messages/en.json';
import zh from './messages/zh.json';
import { eventHelp, eventLabel, loadEventNames } from './event-labels.svelte';
import { CHUNKS } from './event-names';

const GOLDEN = resolve(process.cwd(), '../crates/lodestar/src/settings/golden/logs_catalog.txt');
type Item = { id: string; component: string; label_key: string; help_key: string };
const events = (JSON.parse(readFileSync(GOLDEN, 'utf8').split('\n')[1]!) as { events: Item[] })
	.events;
// Every component's chunk, loaded; an id belongs to its catalogued component's chunk only.
const NAMES: Record<string, [() => string, () => string]> = {};
const CHUNK_NAMES: Record<string, Record<string, unknown>> = {};
const key = (id: string, part: 'n' | 'h') =>
	`settings_logging_ev_${id.replaceAll('.', '_')}_${part}`;

describe('event labels', () => {
	beforeAll(async () => {
		for (const [component, load] of Object.entries(CHUNKS)) {
			const { NAMES: chunk } = await load();
			CHUNK_NAMES[component] = chunk;
			Object.assign(NAMES, chunk);
		}
	});

	it('reads a non-empty catalogue', () => expect(events.length).toBeGreaterThan(100));

	it('names every catalogued event, with help, in en and zh', () => {
		for (const e of events) {
			expect(e.label_key, e.id).toBe(`logs.event.${e.id}.label`);
			expect(e.help_key, e.id).toBe(`logs.event.${e.id}.help`);
			expect(NAMES[e.id], `no names for ${e.id}`).toBeDefined();
			for (const part of ['n', 'h'] as const) {
				for (const [loc, file] of [
					['en', en],
					['zh', zh]
				] as const) {
					const text = (file as Record<string, string>)[key(e.id, part)];
					expect(text?.trim(), `${loc} ${key(e.id, part)}`).toBeTruthy();
				}
			}
		}
	});

	it('has one chunk per catalogued component, holding exactly its events', () => {
		const expected: Record<string, string[]> = {};
		for (const e of events) (expected[e.component] ??= []).push(e.id);
		expect(Object.keys(CHUNKS).sort()).toEqual(Object.keys(expected).sort());
		for (const [component, ids] of Object.entries(expected)) {
			expect(Object.keys(CHUNK_NAMES[component]!), component).toEqual(ids);
		}
	});

	it('has no names for events the server no longer lists', () => {
		const ids = new Set(events.map((e) => e.id));
		expect(Object.keys(NAMES).filter((id) => !ids.has(id))).toEqual([]);
	});

	it('shows the id until the names load, and for an unknown event', async () => {
		const e = { id: 'app.start', component: 'system' } as const;
		expect(eventLabel(e)).toBe('app.start');
		expect(eventHelp(e)).toBeUndefined();
		await loadEventNames('system');
		await waitFor(() => expect(eventLabel(e)).toBe('Lodestar started'), { timeout: 10_000 });
		expect(eventHelp(e)).toBe('Lodestar has started.');
		expect(eventLabel({ id: 'brand.new_event', component: 'system' })).toBe('brand.new_event');
		expect(eventHelp({ id: 'brand.new_event', component: 'system' })).toBeUndefined();
	});

	it('shows the id for a component with no chunk', async () => {
		const e = { id: 'app.start', component: 'brand_new' } as unknown as Parameters<
			typeof eventLabel
		>[0];
		await loadEventNames('brand_new');
		await loadEventNames('constructor');
		expect(eventLabel(e)).toBe('app.start');
		expect(eventHelp(e)).toBeUndefined();
	});
});
