// OverrideDialog (CSX-5), BlockThisDialog and RevealNameDialog over a scripted fetch.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import BlockThisDialog from './BlockThisDialog.svelte';
import OverrideDialog from './OverrideDialog.svelte';
import RevealNameDialog from './RevealNameDialog.svelte';
import { mockFetch } from './testing/fetch';
import { HOSTILE, applied, finished, preview, weakening } from './testing/fixtures';
import type { Assessment, Change } from './types';

configure({ defaultHidden: true });

const HASH = 'ab'.repeat(20);
const PW = 'reveal-test-password';

let net: ReturnType<typeof mockFetch>;
beforeEach(() => {
	net = mockFetch();
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
});
afterEach(() => {
	vi.unstubAllGlobals();
	localStorage.clear();
	sessionStorage.clear();
});

/** Each preview job answers by the change it was started with. */
function previewsBy(answer: (change: Change) => Assessment) {
	let n = 0;
	const byJob = new Map<number, Assessment>();
	net.on('POST /filters/preview/jobs', (req) => {
		const job = ++n;
		byJob.set(job, answer(JSON.parse(req.body!).change));
		return { status: 202, body: { job } };
	});
	net.on('GET /filters/preview/jobs/{id}', (req) => ({
		body: finished(byJob.get(Number(req.path.split('/').pop()))!)
	}));
}

const state = () => screen.getByTestId('change-dialog').dataset.state;
const settledIn = (s: string) => waitFor(() => expect(state()).toBe(s), { timeout: 2000 });

describe('OverrideDialog', () => {
	const OVERRIDE = preview({ quarantine: 214, core_caught_30d: 9 });
	const NEUTRAL = preview({ quarantine: 205, core_caught_30d: 0 });

	beforeEach(() =>
		previewsBy((c) =>
			c.op === 'add_override' && c.neutralize
				? weakening({ warning: 'Neutralizing "jailbait"…', preview: NEUTRAL })
				: weakening({ warning: 'Overriding "jailbait"…', preview: OVERRIDE, neutralize: NEUTRAL })
		)
	);

	it('checks the shape only, and previews before any Apply', async () => {
		render(OverrideDialog, { props: { open: true } });
		await screen.findByRole('dialog', { name: 'Add to the override list' });
		expect(screen.queryByTestId('change-apply')).toBeNull();
		await fireEvent.click(screen.getByRole('radio', { name: 'Exact torrent' }));
		await fireEvent.input(screen.getByTestId('override-value'), { target: { value: 'abc' } });
		await fireEvent.click(screen.getByTestId('override-preview'));
		expect(screen.getByTestId('field-error').textContent).toContain('40 hex digits');
		expect(net.to('/filters/preview')).toHaveLength(0);
		await fireEvent.input(screen.getByTestId('override-value'), {
			target: { value: `${HASH} ${'cd'.repeat(20)}` }
		});
		await fireEvent.click(screen.getByTestId('override-preview'));
		await settledIn('confirm');
		expect(JSON.parse(net.to('/filters/preview/jobs')[0]!.body!).change).toEqual({
			op: 'add_override',
			hashes: [HASH, 'cd'.repeat(20)]
		});
	});

	it('a keyword entry: both strengths side by side; switching re-previews for the warning', async () => {
		render(OverrideDialog, { props: { open: true } });
		await screen.findByRole('dialog');
		await fireEvent.input(screen.getByTestId('override-value'), { target: { value: 'jailbait' } });
		await fireEvent.click(screen.getByTestId('override-preview'));
		await settledIn('confirm');
		expect(screen.getByTestId('impact-override-quarantine').textContent?.trim()).toBe('214');
		expect(screen.getByTestId('impact-neutralize-quarantine').textContent?.trim()).toBe('205');
		expect(screen.getByTestId('effect-warning-text').textContent).toContain('Overriding');
		await fireEvent.input(document.querySelector('input[name="password"]')!, {
			target: { value: PW }
		});
		await fireEvent.click(screen.getByRole('radio', { name: 'Neutralize term' }));
		await settledIn('confirm');
		expect(screen.getByTestId('effect-warning-text').textContent).toContain('Neutralizing');
		// The typed password went with the old strength.
		expect(document.querySelector<HTMLInputElement>('input[name="password"]')!.value).toBe('');
		expect(screen.getByTestId('impact-override-quarantine').textContent?.trim()).toBe('214');
		expect(screen.getByTestId('impact-neutralize-quarantine').textContent?.trim()).toBe('205');
		const sent = net.sent
			.filter((r) => r.method === 'POST' && r.path === '/filters/preview/jobs')
			.map((r) => JSON.parse(r.body!).change);
		expect(sent.at(-1)).toEqual({ op: 'add_override', keyword: 'jailbait', neutralize: true });
	});
});

describe('BlockThisDialog', () => {
	const torrent = { hash: HASH, hash_v2: 'ef'.repeat(32), name: 'Some.Show.S01E02.1080p' };

	beforeEach(() => {
		previewsBy(() => ({ ...weakening(), weakening: false, confirm: false, warning: null }));
		net.on('GET /search', { body: { total: 123456, total_approx: true, hits: [] } });
		net.on('POST /filters/apply', { body: applied(weakening(), { id: 4 }) });
	});

	it('offers the narrowest tier first; payload and group disabled with reasons', async () => {
		render(BlockThisDialog, { props: { torrent, open: true } });
		await screen.findByRole('dialog', { name: 'Block this torrent' });
		const radios = screen.getAllByRole('radio').slice(0, 4) as HTMLInputElement[];
		expect(radios.map((r) => r.value)).toEqual(['torrent', 'payload', 'group', 'phrase']);
		expect(radios.map((r) => r.disabled)).toEqual([false, true, true, false]);
		expect(radios[0]!.checked).toBe(true);
		expect(radios[3]!.checked).toBe(false);
		expect(screen.getByTestId('block-tier-payload').textContent).toContain('payload signature');
		expect(screen.getByTestId('block-query').textContent).toBe(
			`hash:${HASH} | hash:${'ef'.repeat(20)}`
		);
		expect((screen.getByTestId('block-action-hide') as HTMLInputElement).checked).toBe(true);
		await waitFor(() => expect(screen.getByTestId('block-matches').textContent).toContain('≈'));
		const search = new URL(net.to('/search')[0]!.url, 'http://x');
		expect(Object.fromEntries(search.searchParams)).toMatchObject({
			hidden: 'true',
			exact: 'true',
			limit: '1'
		});
	});

	it('a phrase from consecutive words, then the normal flow with addRule', async () => {
		render(BlockThisDialog, { props: { torrent, open: true } });
		await screen.findByRole('dialog');
		await fireEvent.click(screen.getByRole('radio', { name: 'A phrase from the name' }));
		expect(screen.queryByTestId('block-query')).toBeNull();
		const words = screen.getAllByTestId('block-word');
		await fireEvent.click(words[1]!);
		await fireEvent.click(words[2]!);
		expect(screen.getByTestId('block-query').textContent).toBe('name:"Show S01E02"');
		await fireEvent.click(screen.getByTestId('block-action-quarantine'));
		await fireEvent.click(screen.getByTestId('block-preview'));
		await settledIn('review');
		expect(JSON.parse(net.to('/filters/preview/jobs')[0]!.body!).change).toEqual({
			op: 'add_rule',
			query: 'name:"Show S01E02"',
			action: 'quarantine'
		});
	});
});

describe('RevealNameDialog', () => {
	it('shows the sealed name as text, only in the dialog, and forgets it on close', async () => {
		const spies = (['log', 'info', 'warn', 'error', 'debug'] as const).map((k) =>
			vi.spyOn(console, k)
		);
		previewsBy(() => weakening({ warning: 'Revealing shows this torrent’s sealed name.' }));
		net.on('POST /filters/apply', { body: applied(weakening(), { name: HOSTILE }) });
		const onclose = vi.fn();
		render(RevealNameDialog, { props: { hash: HASH, open: true, onclose } });
		await settledIn('confirm');
		expect(JSON.parse(net.to('/filters/preview/jobs')[0]!.body!).change).toEqual({
			op: 'reveal_name',
			hash: HASH
		});
		await fireEvent.input(document.querySelector('input[name="password"]')!, {
			target: { value: PW }
		});
		await fireEvent.submit(document.querySelector('input[name="password"]')!.form!);
		const name = await screen.findByTestId('reveal-name');
		expect(name.textContent).toBe(HOSTILE);
		expect(document.querySelector('img, script')).toBeNull();
		expect(screen.queryByRole('button', { name: /copy/i })).toBeNull();
		expect(document.title).not.toContain('evil');
		await fireEvent.click(screen.getByTestId('change-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(onclose).toHaveBeenCalled();
		expect(document.body.innerHTML).not.toContain('evil');
		const stored = JSON.stringify({ ...localStorage }) + JSON.stringify({ ...sessionStorage });
		for (const secret of [PW, 'evil']) {
			expect(stored).not.toContain(secret);
			expect(location.href).not.toContain(secret);
			for (const s of spies) expect(JSON.stringify(s.mock.calls)).not.toContain(secret);
		}
		for (const s of spies) s.mockRestore();
	});
});
