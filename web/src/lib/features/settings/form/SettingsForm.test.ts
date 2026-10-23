import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { tick } from 'svelte';
import { keys } from '$lib/keys';
import { dismissAll } from './dialogs';
import { loadControl } from './controls.svelte';
import { createSettingsForm, preloadSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';
import type { FieldSpec } from './types';

const nav = vi.hoisted(() => ({
	guards: [] as ((n: unknown) => void)[],
	goto: vi.fn(async () => {})
}));
const hard = vi.hoisted(() => vi.fn());
vi.mock('./leave', async (actual) => ({
	...(await actual<typeof import('./leave')>()),
	hardNavigate: hard
}));
vi.mock('$app/navigation', () => ({
	beforeNavigate: (f: (n: unknown) => void) => nav.guards.push(f),
	goto: nav.goto
}));

const DRAFT_KEY = 'lodestar.settings.draft.v1.harness';

const SPECS: FieldSpec[] = [
	spec('crawler.timeout_secs', { kind: 'integer', minimum: 1, maximum: 120, unit: 'seconds' }),
	spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced', 'aggressive'] }),
	spec('network.http_bind', { format: 'socket-addr', danger: 'disruptive' }),
	spec('crawler.encryption', { kind: 'enum', enum: ['prefer', 'require'], widget: 'radio' }),
	spec('crawler.paused', { kind: 'boolean' })
];

let server: ReturnType<typeof fakeServer>;
let form: SettingsForm;

// The lazily loaded dialog, menu and engine parts, loaded once up front rather than inside a
// test's timeout (`lazy.test.ts` covers loading them on demand).
beforeAll(async () => {
	await Promise.all([
		import('./ConfirmDialog.svelte'),
		import('./FieldMenu.svelte'),
		preloadSettingsForm(),
		...(['secret', 'chips', 'json'] as const).map((w) => loadControl(w))
	]);
});

beforeEach(async () => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	sessionStorage.clear();
	nav.guards.length = 0;
	nav.goto.mockClear();
	hard.mockClear();
	server = fakeServer([
		values('crawler', 3, {
			timeout_secs: sv(12, 'db'),
			intensity: sv('balanced'),
			encryption: sv('prefer'),
			paused: sv(false)
		}),
		values('network', 2, { http_bind: sv('0.0.0.0:8090', 'file') })
	]);
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({ id: 'harness', fields: SPECS });
	await waitFor(() => expect(form.status).toBe('ready'));
});

afterEach(() => {
	dismissAll();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

const patches = () => server.requests('PATCH', '/settings');
const bar = () => screen.queryByTestId('save-bar');

describe('SettingsForm', () => {
	it('shows the bar only with something unsaved, with an exact count', async () => {
		render(FormHarness, { form });
		expect(bar()).toBeNull();
		form.set('crawler.intensity', 'low');
		await waitFor(() =>
			expect(screen.getByTestId('save-bar-status').textContent).toContain('1 unsaved change')
		);
		form.set('crawler.timeout_secs', 30);
		await waitFor(() =>
			expect(screen.getByTestId('save-bar-status').textContent).toContain('2 unsaved changes')
		);
		form.set('crawler.timeout_secs', 12);
		await waitFor(() =>
			expect(screen.getByTestId('save-bar-status').textContent).toContain('1 unsaved change')
		);
	});

	it('saves from the bar, then offers Undo', async () => {
		render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		await waitFor(() =>
			expect(screen.getByTestId('save-bar-status').textContent).toContain('Saved')
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		await waitFor(() => expect(patches()).toHaveLength(2));
	});

	it('never disables Save: with errors it shows the summary, which takes focus', async () => {
		const onReveal = vi.fn();
		render(FormHarness, { form, onReveal });
		form.set('crawler.timeout_secs', 500);
		const save = await screen.findByRole('button', { name: 'Save changes' });
		expect((save as HTMLButtonElement).disabled).toBe(false);
		expect(save.getAttribute('aria-disabled')).not.toBe('true');
		await fireEvent.click(save);
		const summary = await screen.findByTestId('error-summary');
		await waitFor(() => expect(document.activeElement).toBe(within(summary).getByRole('heading')));
		expect(patches()).toHaveLength(0);
		// Its link moves focus to the field's control, after the screen reveals it.
		const link = within(summary).getByRole('link', { name: /timeout_secs: Must be between/ });
		await fireEvent.click(link);
		await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText('timeout_secs')));
		expect(onReveal).toHaveBeenCalledWith('crawler.timeout_secs');
	});

	it('discards one change at once, and asks before discarding more', async () => {
		render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		await fireEvent.click(await screen.findByRole('button', { name: 'Discard' }));
		await waitFor(() => expect(form.dirtyCount).toBe(0));
		expect(screen.queryByRole('dialog')).toBeNull();
		form.set('crawler.intensity', 'low');
		form.set('crawler.timeout_secs', 30);
		await fireEvent.click(await screen.findByRole('button', { name: 'Discard' }));
		const dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Keep editing' }));
		expect(form.dirtyCount).toBe(2);
	});

	it('saves on Ctrl+S inside a field, once, with the registry attached', async () => {
		const detach = keys.attach(window);
		render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		const input = screen.getByLabelText('timeout_secs');
		await waitFor(() =>
			expect(keys.list().some((c) => c.id === 'settings.save.harness')).toBe(true)
		);
		const e = new KeyboardEvent('keydown', {
			key: 's',
			ctrlKey: true,
			bubbles: true,
			cancelable: true
		});
		const saves = vi.spyOn(form, 'save');
		input.dispatchEvent(e);
		expect(e.defaultPrevented).toBe(true);
		await waitFor(() => expect(patches()).toHaveLength(1));
		expect(saves).toHaveBeenCalledTimes(1);
		// Outside the form, the registry's command saves.
		const outside = new KeyboardEvent('keydown', {
			key: 's',
			ctrlKey: true,
			bubbles: true,
			cancelable: true
		});
		document.body.dispatchEvent(outside);
		expect(outside.defaultPrevented).toBe(true);
		expect(saves).toHaveBeenCalledTimes(2);
		detach();
	});

	it('saves on Ctrl+S before the registry has loaded', async () => {
		render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		await fireEvent.keyDown(screen.getByLabelText('timeout_secs'), { key: 's', ctrlKey: true });
		await waitFor(() => expect(patches()).toHaveLength(1));
	});

	it('opens the disruptive confirmation, previewed first, naming the new address', async () => {
		server.onPreview(() => ({
			status: 200,
			body: {
				sections: {
					network: {
						revision: 2,
						fields: {
							http_bind: {
								estimates: [{ name: 'url', value: 'http://192.168.1.5:9000/' }],
								restart_required: true,
								value: '0.0.0.0:9000',
								warnings: []
							}
						}
					}
				}
			}
		}));
		render(FormHarness, { form });
		form.set('network.http_bind', '0.0.0.0:9000');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		const dialog = await screen.findByRole('dialog');
		expect(dialog.textContent).toContain(
			'Lodestar will be at http://192.168.1.5:9000/ after a restart.'
		);
		expect(patches()).toHaveLength(0);
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
		await waitFor(() => expect(patches()).toHaveLength(1));
	});

	it('guards leaving: Keep editing stays, Discard and Save continue', async () => {
		render(FormHarness, { form });
		const guard = nav.guards.at(-1)!;
		const go = (to: string, type = 'link') => {
			const cancel = vi.fn();
			guard({
				type,
				from: { url: new URL('/settings/crawler', location.href) },
				to: { url: new URL(to, location.href) },
				cancel
			});
			return cancel;
		};
		expect(go('/search')).not.toHaveBeenCalled(); // nothing unsaved
		form.set('crawler.intensity', 'low');
		expect(go('/settings/crawler#crawler.intensity')).not.toHaveBeenCalled();
		expect(go('/', 'leave')).toHaveBeenCalled(); // the browser asks

		expect(go('/search')).toHaveBeenCalled();
		let dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Keep editing' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(nav.goto).not.toHaveBeenCalled();

		expect(go('/search', 'popstate')).toHaveBeenCalled();
		dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
		await waitFor(() =>
			expect(nav.goto).toHaveBeenCalledWith(new URL('/search', location.href).href)
		);
		expect(patches()).toHaveLength(1);
	});

	it('announces in one polite live region', async () => {
		render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		await form.save();
		const region = document.querySelector('[aria-live="polite"]')!;
		await waitFor(() => expect(region.textContent).toContain('Saved.'));
	});

	it('offers a draft from earlier', async () => {
		form.set('crawler.intensity', 'low');
		await waitFor(() => expect(sessionStorage.getItem(DRAFT_KEY)).toContain('"low"'), {
			timeout: 10_000
		});
		form.destroy();
		const again = createSettingsForm({ id: 'harness', fields: SPECS });
		await waitFor(() => expect(again.draft).not.toBeNull());
		render(FormHarness, { form: again });
		await fireEvent.click(screen.getByRole('button', { name: 'Restore' }));
		expect(again.field('crawler.intensity').value).toBe('low');
	});

	it('shows the network banner with Retry', async () => {
		server.next('PATCH /settings', { status: 0, body: null });
		render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		await form.save();
		expect(await screen.findByText(/Couldn't reach Lodestar. Your changes are kept./)).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		await waitFor(() => expect(form.status).toBe('saved'));
	});

	it('hides the bar for the wizard', async () => {
		render(FormHarness, { form, saveBar: false });
		form.set('crawler.intensity', 'low');
		await tick();
		expect(form.dirtyCount).toBe(1);
		expect(bar()).toBeNull();
	});

	it.each([
		['a radio group', 'crawler.encryption', 'radio', 'prefer'],
		['a segmented control', 'crawler.intensity', 'radio', 'balanced'],
		['a toggle', 'crawler.paused', 'switch', 'paused']
	] as const)(
		'moves focus from the summary to %s, never to its menu',
		async (_, key, role, name) => {
			render(FormHarness, { form });
			const [section, field] = key.split('.') as [string, string];
			server.next('PATCH /settings', {
				status: 422,
				body: { error: { code: 'invalid_setting', section, field, message: 'No.' } }
			});
			form.set('crawler.timeout_secs', 30);
			await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
			const summary = await screen.findByTestId('error-summary');
			await fireEvent.click(within(summary).getByRole('link'));
			const root = document.getElementById(key)!;
			await waitFor(() =>
				expect(document.activeElement).toBe(within(root).getByRole(role, { name }))
			);
		}
	);

	it('prevents the browser’s Save page dialog on a held Ctrl+S, saving once', async () => {
		render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		const input = screen.getByLabelText('timeout_secs');
		const saves = vi.spyOn(form, 'save');
		const press = (repeat: boolean) => {
			const e = new KeyboardEvent('keydown', {
				key: 's',
				ctrlKey: true,
				repeat,
				bubbles: true,
				cancelable: true
			});
			input.dispatchEvent(e);
			return e.defaultPrevented;
		};
		expect(press(false)).toBe(true);
		expect(press(true)).toBe(true);
		expect(press(true)).toBe(true);
		expect(saves).toHaveBeenCalledTimes(1);
	});

	it('lets a screen’s own disruptive handler win over the default dialog', async () => {
		const own = vi.fn(async () => true);
		form.gate('disruptive', own);
		render(FormHarness, { form });
		form.set('network.http_bind', '0.0.0.0:9000');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		await waitFor(() => expect(patches()).toHaveLength(1));
		expect(own).toHaveBeenCalledTimes(1);
		expect(screen.queryByRole('dialog')).toBeNull();
	});

	it('continues to another site, or a reload link, with a full-page navigation', async () => {
		render(FormHarness, { form });
		const guard = nav.guards.at(-1)!;
		form.set('crawler.intensity', 'low');
		const leave = (to: string, willUnload: boolean) =>
			guard({
				type: 'link',
				willUnload,
				from: { url: new URL('/settings/crawler', location.href) },
				to: { url: new URL(to, location.href) },
				cancel: vi.fn()
			});
		leave('https://example.com/', true);
		let dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Discard' }));
		await waitFor(() => expect(hard).toHaveBeenCalledWith('https://example.com/'));
		expect(nav.goto).not.toHaveBeenCalled();

		dismissAll();
		form.set('crawler.intensity', 'low');
		leave('/search', true); // data-sveltekit-reload
		dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
		await waitFor(() => expect(hard).toHaveBeenCalledWith(new URL('/search', location.href).href));
	});

	it('continues after Save when nothing is left to save', async () => {
		render(FormHarness, { form });
		const guard = nav.guards.at(-1)!;
		form.set('crawler.intensity', 'low');
		guard({
			type: 'link',
			willUnload: false,
			from: { url: new URL('/settings/crawler', location.href) },
			to: { url: new URL('/search', location.href) },
			cancel: vi.fn()
		});
		const dialog = await screen.findByRole('dialog');
		form.set('crawler.intensity', 'balanced'); // settled meanwhile
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
		await waitFor(() => expect(nav.goto).toHaveBeenCalled());
	});

	it('shows and focuses the summary for a save started elsewhere (the wizard’s Next)', async () => {
		render(FormHarness, { form, saveBar: false });
		form.set('crawler.timeout_secs', 500);
		expect(await form.save()).toEqual({ ok: false, reason: 'invalid' });
		const summary = await screen.findByTestId('error-summary');
		await waitFor(() => expect(document.activeElement).toBe(within(summary).getByRole('heading')));
		form.set('crawler.timeout_secs', 30);
		expect(await form.save()).toMatchObject({ ok: true });
		await waitFor(() => expect(screen.queryByTestId('error-summary')).toBeNull());
	});

	it('closes its open question when it unmounts', async () => {
		const view = render(FormHarness, { form });
		form.set('crawler.intensity', 'low');
		form.set('crawler.timeout_secs', 30);
		await fireEvent.click(await screen.findByRole('button', { name: 'Discard' }));
		await screen.findByRole('dialog');
		view.unmount();
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(form.dirtyCount).toBe(2); // unanswered: nothing discarded
	});
});
