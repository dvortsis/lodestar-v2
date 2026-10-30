// SettingsForm when the edit chunk can't load (the connection dropped, or an update replaced
// the build): edits are still kept as a draft, the form says to reload, a stored draft's offer
// says it couldn't load and is kept, and leaving falls back to the browser's own prompt.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { DRAFT_PREFIX } from './draft-store';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';

const nav = vi.hoisted(() => ({ guards: [] as ((n: unknown) => void)[] }));
const hard = vi.hoisted(() => vi.fn());
vi.mock('./edit', () => {
	throw new Error('the edit chunk failed to load');
});
vi.mock('./leave', async (actual) => ({
	...(await actual<typeof import('./leave')>()),
	hardNavigate: hard
}));
vi.mock('$app/navigation', () => ({
	beforeNavigate: (f: (n: unknown) => void) => nav.guards.push(f),
	goto: async () => {}
}));

let form: SettingsForm;
// Up to 10 s, for the slowest CI runner; each test gets 20.
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

beforeEach(async () => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	sessionStorage.clear();
	nav.guards.length = 0;
	hard.mockClear();
	const server = fakeServer([values('crawler', 3, { intensity: sv('low') })]);
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({
		id: 'offline',
		fields: [spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced'] })]
	});
	await until(() => expect(form.status).toBe('ready'));
});

afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('SettingsForm without its edit chunk', { timeout: 20_000 }, () => {
	it('keeps the edit as a draft and says to reload', async () => {
		render(FormHarness, { form });
		form.set('crawler.intensity', 'balanced');
		await until(() => expect(screen.getByTestId('part-failed')).toBeTruthy());
		expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy();
		await until(() =>
			expect(sessionStorage.getItem(DRAFT_PREFIX + 'offline')).toContain('"balanced"')
		);
		expect(await form.save()).toEqual({ ok: false, reason: 'network' });
	});

	it('says a stored draft couldn’t load, keeps it and offers Retry', async () => {
		const stored = JSON.stringify({
			v: 1,
			at: 1,
			sections: {
				crawler: {
					revision: 3,
					base: { intensity: 'low' },
					mine: { intensity: 'balanced' },
					reset: []
				}
			}
		});
		sessionStorage.setItem(DRAFT_PREFIX + 'drafted', stored);
		const drafted = createSettingsForm({
			id: 'drafted',
			fields: [spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced'] })]
		});
		await until(() => expect(drafted.status).toBe('ready'));
		render(FormHarness, { form: drafted });
		expect(screen.getByTestId('draft-failed')).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
		// One banner: the draft's says what the general one would.
		drafted.set('crawler.intensity', 'balanced');
		// The save bar's load fails on the same promise as the save's, first.
		expect(await drafted.save()).toEqual({ ok: false, reason: 'network' });
		await tick();
		expect(screen.getByTestId('draft-failed')).toBeTruthy();
		expect(screen.queryByTestId('part-failed')).toBeNull();
		drafted.destroy();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'drafted')).toBe(stored);
	});

	it('leaves by a full page load, so the browser’s own prompt asks', async () => {
		render(FormHarness, { form });
		form.set('crawler.intensity', 'balanced');
		nav.guards.at(-1)!({
			type: 'link',
			willUnload: false,
			from: { url: new URL('/settings/crawler', location.href) },
			to: { url: new URL('/search', location.href) },
			cancel: vi.fn()
		});
		await until(() => expect(hard).toHaveBeenCalledWith(new URL('/search', location.href).href));
	});
});
