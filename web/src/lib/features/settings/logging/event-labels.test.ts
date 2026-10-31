// Every event the server catalogues has a plain name and a line of help, in both languages; an
// event the client doesn't know falls back to its id. The list is the server's own golden catalogue,
// so a new server event fails here until it's labelled.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { waitFor } from '@testing-library/svelte';
import en from './messages/en.json';
import zh from './messages/zh.json';
import { eventHelp, eventLabel, loadEventNames } from './event-labels.svelte';
import { NAMES } from './event-names';

const GOLDEN = resolve(process.cwd(), '../crates/lodestar/src/settings/golden/logs_catalog.txt');
type Item = { id: string; label_key: string; help_key: string };
const events = (JSON.parse(readFileSync(GOLDEN, 'utf8').split('\n')[1]!) as { events: Item[] })
	.events;
const key = (id: string, part: 'n' | 'h') =>
	`settings_logging_ev_${id.replaceAll('.', '_')}_${part}`;

describe('event labels', () => {
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

	it('has no names for events the server no longer lists', () => {
		const ids = new Set(events.map((e) => e.id));
		expect(Object.keys(NAMES).filter((id) => !ids.has(id))).toEqual([]);
	});

	it('shows the id until the names load, and for an unknown event', async () => {
		const e = { id: 'app.start' };
		expect(eventLabel(e)).toBe('app.start');
		expect(eventHelp(e)).toBeUndefined();
		await loadEventNames();
		await waitFor(() => expect(eventLabel(e)).toBe('Lodestar started'), { timeout: 10_000 });
		expect(eventHelp(e)).toBe('Lodestar has started.');
		expect(eventLabel({ id: 'brand.new_event' })).toBe('brand.new_event');
		expect(eventHelp({ id: 'brand.new_event' })).toBeUndefined();
	});
});
