// The digest preview: the plain text renders as text and never as markup; loading, error and off.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import DigestPreview from './DigestPreview.svelte';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (actual) => ({
	...(await actual<typeof import('$lib/api/client')>()),
	api: { get }
}));

const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
const saved = (digest: string) => ({ fields: { digest: { value: digest } } });
const press = () => fireEvent.click(screen.getByTestId('digest-preview-load'));

describe('DigestPreview', () => {
	beforeEach(() => {
		get.mockReset();
		initLocale('en');
	});

	it('shows the text version literally, never as markup', async () => {
		get.mockImplementation((path: string) =>
			Promise.resolve(
				path === '/digest/preview'
					? { subject: 'Weekly <i>digest</i>', text: 'Found <b>12</b> new <script>x</script>' }
					: saved('weekly')
			)
		);
		render(DigestPreview);
		await press();
		const pre = await screen.findByTestId('digest-preview-text');
		expect(pre.tagName).toBe('PRE');
		expect(pre.textContent).toBe('Found <b>12</b> new <script>x</script>');
		expect(pre.querySelector('b, script')).toBeNull();
		expect(screen.getByTestId('digest-preview-subject').querySelector('i')).toBeNull();
		expect(get).toHaveBeenCalledWith('/digest/preview', { query: { period: 'weekly' } });
	});

	it('says the digest is off and does not fetch a preview', async () => {
		get.mockResolvedValue(saved('off'));
		render(DigestPreview);
		await press();
		await until(() => expect(screen.getByTestId('digest-preview-off')).toBeTruthy());
		expect(get).toHaveBeenCalledTimes(1);
	});

	it('shows a failure as text', async () => {
		get.mockRejectedValue(new Error('boom'));
		render(DigestPreview);
		await press();
		await until(() =>
			expect(screen.getByTestId('digest-preview-error').textContent).toContain('boom')
		);
	});

	it('asks nothing until pressed, and shows loading while it waits', async () => {
		let release: (v: unknown) => void = () => {};
		get.mockReturnValue(new Promise((r) => (release = r)));
		render(DigestPreview);
		expect(get).not.toHaveBeenCalled();
		await press();
		await until(() => expect(screen.getByText('Loading the preview…')).toBeTruthy());
		release(saved('off'));
		await until(() => expect(screen.getByTestId('digest-preview-off')).toBeTruthy());
	});
});
