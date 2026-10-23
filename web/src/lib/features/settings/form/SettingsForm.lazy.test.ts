// SettingsForm with nothing preloaded: the bar shows with the first edit, and the summary takes
// focus once per failed save, not when it comes back while a field is fixed.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import FormHarness from './testing/FormHarness.svelte';
import { fakeServer } from './testing/fake-server';
import { spec, sv, values } from './testing/values';

vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: async () => {} }));

let form: SettingsForm;
// Up to 10 s, for the slowest CI runner; each test gets 20.
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

beforeEach(async () => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	const server = fakeServer([values('crawler', 3, { timeout_secs: sv(12), intensity: sv('low') })]);
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({
		id: 'cold',
		fields: [
			spec('crawler.timeout_secs', { kind: 'integer', minimum: 1, maximum: 120 }),
			spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced'] })
		]
	});
	await until(() => expect(form.status).toBe('ready'));
});

afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('SettingsForm, cold', { timeout: 20_000 }, () => {
	it('shows the bar with the first edit, once its code is in', async () => {
		render(FormHarness, { form });
		expect(screen.queryByTestId('save-bar')).toBeNull();
		form.set('crawler.intensity', 'balanced');
		await until(() =>
			expect(screen.getByTestId('save-bar-status').textContent).toContain('1 unsaved change')
		);
	});

	it('focuses the summary once per failed save, not when an error comes back', async () => {
		render(FormHarness, { form });
		form.set('crawler.timeout_secs', 500);
		await fireEvent.click(await screen.findByRole('button', { name: 'Save changes' }));
		const summary = await screen.findByTestId('error-summary');
		await until(() => expect(document.activeElement).toBe(within(summary).getByRole('heading')));
		// Fixing the field hides the summary; a new bad value brings the error and the summary back.
		const input = screen.getByLabelText('timeout_secs');
		input.focus();
		form.set('crawler.timeout_secs', 50);
		await until(() => expect(screen.queryByTestId('error-summary')).toBeNull());
		form.set('crawler.timeout_secs', 0);
		form.blur('crawler.timeout_secs'); // its error is back
		await until(() => expect(screen.getByTestId('error-summary')).toBeTruthy());
		expect(document.activeElement).toBe(input);
		// A new failed save takes it again.
		await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
		await until(() =>
			expect(document.activeElement).toBe(
				within(screen.getByTestId('error-summary')).getByRole('heading')
			)
		);
	});
});
