import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { keys } from '$lib/keys';
import { dismissAll } from './dialogs';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';
import type { FieldSpec } from './types';

const nav = vi.hoisted(() => ({
	guards: [] as ((n: unknown) => void)[],
	goto: vi.fn(async () => {})
}));
vi.mock('$app/navigation', () => ({
	beforeNavigate: (f: (n: unknown) => void) => nav.guards.push(f),
	goto: nav.goto
}));

const SPECS: FieldSpec[] = [
	spec('crawler.timeout_secs', { kind: 'integer', minimum: 1, maximum: 120, unit: 'seconds' }),
	spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced', 'aggressive'] }),
	spec('network.http_bind', { format: 'socket-addr', danger: 'disruptive' })
];

let server: ReturnType<typeof fakeServer>;
let form: SettingsForm;

// The lazily loaded dialog and menu, transformed once up front rather than inside a test's timeout.
beforeAll(async () => {
	await Promise.all([import('./ConfirmDialog.svelte'), import('./FieldMenu.svelte')]);
});

beforeEach(async () => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	sessionStorage.clear();
	nav.guards.length = 0;
	nav.goto.mockClear();
	server = fakeServer([
		values('crawler', 3, { timeout_secs: sv(12, 'db'), intensity: sv('balanced') }),
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
				from: { url: new URL('http://x/settings/crawler') },
				to: { url: new URL(to, 'http://x') },
				cancel
			});
			return cancel;
		};
		expect(go('/search')).not.toHaveBeenCalled(); // nothing unsaved
		form.set('crawler.intensity', 'low');
		expect(go('/settings/crawler#crawler.intensity')).not.toHaveBeenCalled();
		expect(go('http://x/', 'leave')).toHaveBeenCalled(); // the browser asks

		expect(go('/search')).toHaveBeenCalled();
		let dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Keep editing' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(nav.goto).not.toHaveBeenCalled();

		expect(go('/search', 'popstate')).toHaveBeenCalled();
		dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
		await waitFor(() => expect(nav.goto).toHaveBeenCalledWith('http://x/search'));
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
		await new Promise((r) => setTimeout(r, 350));
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
		await new Promise((r) => setTimeout(r, 10));
		expect(bar()).toBeNull();
	});
});
