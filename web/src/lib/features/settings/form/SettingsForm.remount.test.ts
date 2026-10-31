import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { dismissAll } from './dialogs';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';

vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));

const SPECS = [
	spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced', 'aggressive'] })
];
let server: ReturnType<typeof fakeServer>;
const region = () => document.querySelector('.ls-form') as HTMLElement;

beforeEach(() => {
	sessionStorage.clear();
	server = fakeServer([values('crawler', 3, { intensity: sv('balanced') })]);
	vi.stubGlobal('fetch', server.fetch);
});
afterEach(() => {
	dismissAll();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

async function shown(): Promise<SettingsForm> {
	const form = createSettingsForm({ id: 'remount', fields: SPECS });
	render(FormHarness, { form });
	await screen.findByRole('radio', { name: 'balanced' });
	return form;
}

describe('SettingsForm keeps its body through a reload', () => {
	it('shows the loading skeleton on the first load, with no body', async () => {
		const release = server.hold('GET /settings/crawler');
		const form = createSettingsForm({ id: 'remount', fields: SPECS });
		render(FormHarness, { form });
		expect(screen.getByText('Loading settings…')).toBeTruthy();
		expect(screen.queryByRole('radio')).toBeNull();
		release();
		await screen.findByRole('radio', { name: 'balanced' });
	});

	it('keeps the same elements mounted and marks the region busy while it reloads', async () => {
		const form = await shown();
		const radio = screen.getByRole('radio', { name: 'balanced' });
		radio.focus();
		const release = server.hold('GET /settings/crawler');
		void reload(form);
		await waitFor(() => expect(region().getAttribute('aria-busy')).toBe('true'));
		expect(form.status).toBe('loading');
		expect(screen.getByRole('radio', { name: 'balanced' })).toBe(radio);
		expect(document.activeElement).toBe(radio);
		expect(screen.queryByText('Loading settings…')).toBeNull();
		release();
		await waitFor(() => expect(region().getAttribute('aria-busy')).toBeNull());
		expect(screen.getByRole('radio', { name: 'balanced' })).toBe(radio);
		expect(document.activeElement).toBe(radio);
	});

	it('keeps the body and shows the error with Retry when a reload fails', async () => {
		const form = await shown();
		const radio = screen.getByRole('radio', { name: 'balanced' });
		server.next('GET /settings/crawler', server.err(500, 'internal'));
		void reload(form);
		await waitFor(() => expect(form.status).toBe('load-error'));
		expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
		expect(screen.getByRole('radio', { name: 'balanced' })).toBe(radio);
		await form.retry();
		await waitFor(() => expect(form.status).toBe('ready'));
		expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
		expect(screen.getByRole('radio', { name: 'balanced' })).toBe(radio);
	});
});

// `load` isn't on the public type: only the engine calls it (first load, Retry).
const reload = (form: SettingsForm) => (form as unknown as { load(): Promise<void> }).load();
