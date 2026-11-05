// The Legal Notice step on its own: it renders the server's notice as nodes (never as HTML), the
// box starts unticked and is required, errors are linked to it, and it loads the notice only once.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { resetNoticeCache } from '../auth/notice';
import LegalStep from './LegalStep.svelte';

const NOTICE = {
	version: 2,
	text: [
		'# Lodestar Legal Notice',
		'',
		'**Version 2 · 2026-11-04**',
		'',
		'Read <script>alert(1)</script> and <img src=x onerror=alert(1)> as text.',
		'',
		'- See https://github.com/dvortsis/lodestar-v2.',
		'- Not a link: javascript:alert(1) or http://example.org'
	].join('\n')
};

let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
	resetNoticeCache();
	fetchMock = vi.fn(
		async () =>
			new Response(JSON.stringify(NOTICE), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			})
	);
	vi.stubGlobal('fetch', fetchMock);
});
afterEach(() => vi.unstubAllGlobals());

const box = () => screen.getByLabelText('I have read the Legal Notice') as HTMLInputElement;

describe('LegalStep', () => {
	it('shows the notice as text and nodes, never as markup', async () => {
		render(LegalStep);
		const region = await screen.findByRole('region', { name: 'Legal Notice' });
		expect(region.querySelector('script, img')).toBeNull();
		expect(region.textContent).toContain('<script>alert(1)</script>');
		const links = [...region.querySelectorAll('a')];
		expect(links.map((a) => a.getAttribute('href'))).toEqual([
			'https://github.com/dvortsis/lodestar-v2'
		]);
		expect(links[0]!.rel).toBe('noopener noreferrer');
		expect(links[0]!.target).toBe('_blank');
	});

	it('starts unticked and required, and the box waits for the notice', async () => {
		let release!: (r: Response) => void;
		fetchMock.mockImplementationOnce(() => new Promise<Response>((r) => (release = r)));
		render(LegalStep);
		expect(screen.getByRole('status').textContent).toContain('Loading the Legal Notice');
		// Drawn at once so nothing moves when the notice arrives, but not tickable yet.
		expect(box().disabled).toBe(true);
		release(new Response(JSON.stringify(NOTICE), { status: 200 }));
		await screen.findByRole('region', { name: 'Legal Notice' });
		expect(box().disabled).toBe(false);
		expect(box().checked).toBe(false);
		expect(box().required).toBe(true);
		expect(box().getAttribute('aria-invalid')).toBeNull();
	});

	it('links each error to the box', async () => {
		const { rerender } = render(LegalStep, { error: 'unchecked' });
		await screen.findByRole('region', { name: 'Legal Notice' });
		expect(box().getAttribute('aria-invalid')).toBe('true');
		const id = box().getAttribute('aria-describedby')!;
		expect(document.getElementById(id)!.textContent).toContain(
			'Confirm that you have read the Legal Notice'
		);
		await rerender({ error: 'changed' });
		expect(document.getElementById(id)!.textContent).toContain('changed while this page was open');
	});

	it('focuses the notice once it has loaded, when asked', async () => {
		render(LegalStep, { focusOnLoad: true });
		const region = await screen.findByRole('region', { name: 'Legal Notice' });
		await waitFor(() => expect(document.activeElement).toBe(region));
		expect(region.getAttribute('tabindex')).toBe('0');
	});

	it('uses the notice it was given without fetching again', async () => {
		render(LegalStep, { notice: NOTICE });
		expect(box().disabled).toBe(false);
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
