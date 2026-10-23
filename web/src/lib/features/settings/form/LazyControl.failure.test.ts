// A control whose code can't load (the connection dropped, or an update replaced the build): the
// field says so in the control's place, nothing is left holding its space, and the rest of the
// form still saves. Its own module graph, with the number control's import failing.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import SettingField from './SettingField.svelte';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';

vi.mock('./NumberControl.svelte', () => {
	throw new Error('the number control failed to load');
});

let form: SettingsForm;
let server: ReturnType<typeof fakeServer>;
// Up to 10 s, for the slowest CI runner; each test gets 20.
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

beforeEach(async () => {
	server = fakeServer([values('crawler', 1, { timeout_secs: sv(12), paused: sv(false) })]);
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({
		id: 'control-failed',
		fields: [
			spec('crawler.timeout_secs', { kind: 'integer', minimum: 1, maximum: 120 }),
			spec('crawler.paused', { kind: 'boolean' })
		]
	});
	await until(() => expect(form.status).toBe('ready'));
});

afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
});

describe('a control that can’t load', { timeout: 20_000 }, () => {
	it('says so in its place, and the other fields still save', async () => {
		render(SettingField, { form, key: 'crawler.timeout_secs' });
		render(SettingField, { form, key: 'crawler.paused' });
		const line = await screen.findByText(
			/Part of this page couldn't load/,
			{},
			{ timeout: 10_000 }
		);
		expect(document.querySelector('[data-placeholder]')).toBeNull();
		// Its group is labelled by the field and described by the line.
		const group = line.closest('[role="group"]')!;
		expect(group.getAttribute('aria-describedby')).toBe(line.id);
		form.set('crawler.paused', true);
		expect(await form.save()).toMatchObject({ ok: true });
		expect(server.requests('PATCH', '/settings').map((r) => r.body)).toEqual([
			{ crawler: { if_revision: 1, paused: true } }
		]);
	});
});
