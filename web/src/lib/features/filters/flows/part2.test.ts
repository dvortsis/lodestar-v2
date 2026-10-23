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
		await fireEvent.submit(
			document.querySelector<HTMLInputElement>('input[name="password"]')!.form!
		);
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

describe('OverrideDialog: edges', () => {
	const NEUTRAL = preview({ quarantine: 205 });
	const posts = () =>
		net.sent
			.filter((r) => r.method === 'POST' && r.path === '/filters/preview/jobs')
			.map((r) => JSON.parse(r.body!).change);

	it('checks payload and days shape, and sends scope, expiry and shadow', async () => {
		previewsBy(() => weakening());
		render(OverrideDialog, { props: { open: true } });
		await screen.findByRole('dialog');
		await fireEvent.click(screen.getByRole('radio', { name: 'Payload signature' }));
		await fireEvent.input(screen.getByTestId('override-value'), { target: { value: 'abc' } });
		await fireEvent.input(screen.getByTestId('override-expires'), { target: { value: '0' } });
		await fireEvent.click(screen.getByTestId('override-preview'));
		const errors = screen.getAllByTestId('field-error').map((e) => e.textContent);
		expect(errors.join(' ')).toContain('16 hex digits');
		expect(errors.join(' ')).toContain('3,650');
		expect(posts()).toHaveLength(0);
		await fireEvent.input(screen.getByTestId('override-value'), {
			target: { value: '0123456789ABCDEF' }
		});
		await fireEvent.input(screen.getByTestId('override-expires'), { target: { value: '30' } });
		await fireEvent.input(screen.getByTestId('override-shadow'), { target: { value: '7' } });
		await fireEvent.input(screen.getByTestId('override-scope'), {
			target: { value: 'type:adult' }
		});
		await fireEvent.click(screen.getByTestId('override-preview'));
		await settledIn('confirm');
		expect(posts()[0]).toEqual({
			op: 'add_override',
			payload: '0123456789abcdef',
			scope: 'type:adult',
			expires_days: 30,
			shadow_days: 7
		});
	});

	it('a shadow choice made in the dialog survives a strength switch', async () => {
		previewsBy((c) =>
			c.op === 'add_override' && c.neutralize
				? weakening({ preview: NEUTRAL })
				: weakening({ neutralize: NEUTRAL })
		);
		render(OverrideDialog, { props: { open: true, shadowDays: 7 } });
		await screen.findByRole('dialog');
		await fireEvent.input(screen.getByTestId('override-value'), { target: { value: 'jailbait' } });
		await fireEvent.click(screen.getByTestId('override-preview'));
		await settledIn('confirm');
		await fireEvent.click(screen.getByTestId('change-shadow'));
		await waitFor(() => expect(posts()).toHaveLength(2));
		await settledIn('confirm');
		await fireEvent.click(screen.getByRole('radio', { name: 'Neutralize term' }));
		await waitFor(() => expect(posts()).toHaveLength(3));
		expect(posts()[2]).toEqual({
			op: 'add_override',
			keyword: 'jailbait',
			shadow_days: 7,
			neutralize: true
		});
		await settledIn('confirm');
		expect(screen.getByTestId('impact-neutralize-heading').textContent).toContain('(chosen)');
	});

	it('offers Neutralize after the preview only when the server previewed it', async () => {
		previewsBy(() => weakening({ neutralize: null }));
		render(OverrideDialog, { props: { open: true } });
		await screen.findByRole('dialog');
		await fireEvent.input(screen.getByTestId('override-value'), {
			target: { value: 'two words' }
		});
		await fireEvent.click(screen.getByTestId('override-preview'));
		await settledIn('confirm');
		expect(screen.queryByRole('radio', { name: 'Neutralize term' })).toBeNull();
	});

	it('a refused preview offers Back, with what was typed', async () => {
		net.on('POST /filters/preview/jobs', { status: 202, body: { job: 1 } });
		net.on('GET /filters/preview/jobs/{id}', {
			body: {
				state: 'failed',
				status: 400,
				error: { code: 'invalid_override', message: 'Neutralize term takes one word.' },
				progress: { phase: 'reading', done: 0, total: 0 }
			}
		});
		render(OverrideDialog, { props: { open: true } });
		await screen.findByRole('dialog');
		await fireEvent.click(screen.getByRole('radio', { name: 'Neutralize term' }));
		await fireEvent.input(screen.getByTestId('override-value'), {
			target: { value: 'two words' }
		});
		await fireEvent.click(screen.getByTestId('override-preview'));
		await settledIn('failed');
		expect(screen.getByTestId('change-error').textContent).toBe('Neutralize term takes one word.');
		await fireEvent.click(screen.getByTestId('change-back'));
		await screen.findByTestId('override-form');
		expect((screen.getByTestId('override-value') as HTMLInputElement).value).toBe('two words');
		expect(screen.getByRole('dialog')).toBeTruthy();
	});
});

describe('BlockThisDialog: edges', () => {
	it('a v1-only torrent blocks one hash; the count failing says so', async () => {
		previewsBy(() => weakening());
		net.error('GET /search', 500, 'internal');
		render(BlockThisDialog, {
			props: { torrent: { hash: HASH, hash_v2: null, name: 'A B C D' }, open: true }
		});
		await screen.findByRole('dialog');
		expect(screen.getByTestId('block-query').textContent).toBe(`hash:${HASH}`);
		await waitFor(() =>
			expect(screen.getByTestId('block-matches').textContent).toContain("Couldn't count")
		);
	});

	it('a bad hash: the torrent tier says why it is unavailable', async () => {
		net.on('GET /search', { body: { total: 0, total_approx: false, hits: [] } });
		render(BlockThisDialog, {
			props: { torrent: { hash: 'xyz', hash_v2: null, name: 'A B' }, open: true }
		});
		await screen.findByRole('dialog');
		expect(screen.getByTestId('block-tier-torrent').textContent).toContain("isn't valid");
		expect(screen.queryByTestId('block-query')).toBeNull();
	});

	it('the word run extends, shrinks from an end, and restarts elsewhere', async () => {
		net.on('GET /search', { body: { total: 2, total_approx: false, hits: [] } });
		render(BlockThisDialog, {
			props: { torrent: { hash: HASH, hash_v2: null, name: 'A B C D E' }, open: true }
		});
		await screen.findByRole('dialog');
		await fireEvent.click(screen.getByRole('radio', { name: 'A phrase from the name' }));
		const w = () => screen.getAllByTestId('block-word');
		const q = () => screen.queryByTestId('block-query')?.textContent;
		await fireEvent.click(w()[1]!);
		await fireEvent.click(w()[2]!);
		await fireEvent.click(w()[3]!);
		expect(q()).toBe('name:"B C D"');
		await fireEvent.click(w()[1]!);
		expect(q()).toBe('name:"C D"');
		await fireEvent.click(w()[0]!);
		expect(q()).toBe('name:"A"');
		await fireEvent.click(w()[0]!);
		expect(q()).toBeUndefined();
		expect(w()[0]!.getAttribute('aria-pressed')).toBe('false');
	});

	it('shows the match count again where Apply is', async () => {
		previewsBy(() => ({ ...weakening(), confirm: false, warning: null }));
		net.on('GET /search', { body: { total: 3, total_approx: false, hits: [] } });
		render(BlockThisDialog, {
			props: { torrent: { hash: HASH, hash_v2: null, name: 'A' }, open: true }
		});
		await screen.findByRole('dialog');
		await waitFor(() => expect(screen.getByTestId('block-matches').textContent).toContain('3'));
		await fireEvent.click(screen.getByTestId('block-preview'));
		await settledIn('review');
		expect(screen.getByTestId('block-matches').textContent).toContain('3');
	});
});

describe('RevealNameDialog: edges', () => {
	it('a wrong password, then the right one', async () => {
		previewsBy(() => weakening());
		net.on(
			'POST /filters/apply',
			{ status: 401, body: { error: { code: 'bad_password', message: 'x', field: 'password' } } },
			{ body: applied(weakening(), { name: 'The.Name' }) }
		);
		render(RevealNameDialog, { props: { hash: HASH, open: true } });
		await settledIn('confirm');
		const pw = () => document.querySelector<HTMLInputElement>('input[name="password"]')!;
		await fireEvent.input(pw(), { target: { value: 'wrong' } });
		await fireEvent.submit(pw().form!);
		await waitFor(() => expect(screen.getByTestId('field-error').textContent).toContain('wrong'));
		expect(pw().value).toBe('');
		await fireEvent.input(pw(), { target: { value: PW } });
		await fireEvent.submit(pw().form!);
		expect((await screen.findByTestId('reveal-name')).textContent).toBe('The.Name');
	});

	it('a different hash while open previews that one; the old name is gone', async () => {
		previewsBy(() => weakening());
		net.on('POST /filters/apply', { body: applied(weakening(), { name: 'First.Name' }) });
		const r = render(RevealNameDialog, { props: { hash: HASH, open: true } });
		await settledIn('confirm');
		const pw = () => document.querySelector<HTMLInputElement>('input[name="password"]')!;
		await fireEvent.input(pw(), { target: { value: PW } });
		await fireEvent.submit(pw().form!);
		await screen.findByTestId('reveal-name');
		await r.rerender({ hash: 'cd'.repeat(20), open: true });
		await settledIn('confirm');
		expect(document.body.textContent).not.toContain('First.Name');
		const last = net.sent
			.filter((s) => s.method === 'POST' && s.path === '/filters/preview/jobs')
			.at(-1)!;
		expect(JSON.parse(last.body!).change).toEqual({ op: 'reveal_name', hash: 'cd'.repeat(20) });
	});
});

describe('runner review of part 2', () => {
	it('a strength switch made mid-preview waits, then re-previews the strength on screen', async () => {
		// Each job answers by its own change; job polls stay pending until released.
		const jobs: Change[] = [];
		const gates: (() => void)[] = [];
		net.on('POST /filters/preview/jobs', (req) => {
			jobs.push(JSON.parse(req.body!).change);
			return { status: 202, body: { job: jobs.length } };
		});
		net.on('GET /filters/preview/jobs/{id}', async (req) => {
			const id = Number(req.path.split('/').pop());
			const c = jobs[id - 1]!;
			if (id === 2) await new Promise<void>((r) => gates.push(r));
			return {
				body: finished(
					c.op === 'add_override' && c.neutralize
						? weakening({
								warning: 'Neutralizing "jailbait"…',
								preview: preview({ quarantine: 205 })
							})
						: weakening({
								warning: 'Overriding "jailbait"…',
								neutralize: preview({ quarantine: 205 })
							})
				)
			};
		});
		net.on('POST /filters/apply', { body: applied(weakening(), { id: 3 }) });
		render(OverrideDialog, { props: { open: true } });
		await screen.findByRole('dialog');
		await fireEvent.input(screen.getByTestId('override-value'), { target: { value: 'jailbait' } });
		await fireEvent.click(screen.getByTestId('override-preview'));
		await settledIn('confirm');
		await fireEvent.click(screen.getByRole('radio', { name: 'Neutralize term' }));
		await settledIn('previewing');
		await fireEvent.click(screen.getByRole('radio', { name: 'Override' }));
		// The Neutralize preview now finishes; the strength on screen is Override again.
		await waitFor(() => expect(gates.length).toBe(1));
		gates[0]!();
		await waitFor(() => expect(jobs).toHaveLength(3));
		await settledIn('confirm');
		expect(screen.getByTestId('effect-warning-text').textContent).toContain('Overriding');
		expect(jobs[2]).toEqual({ op: 'add_override', keyword: 'jailbait' });
		await fireEvent.input(document.querySelector('input[name="password"]')!, {
			target: { value: PW }
		});
		await fireEvent.submit(
			document.querySelector<HTMLInputElement>('input[name="password"]')!.form!
		);
		await screen.findByText('What changed');
		const sent = JSON.parse(net.to('/filters/apply')[0]!.body!);
		expect(sent.change).toEqual({ op: 'add_override', keyword: 'jailbait' });
	});

	it('reveal closes when its flow ends by itself', async () => {
		net.on('POST /filters/preview/jobs', { status: 202, body: { job: 1 } });
		net.on('GET /filters/preview/jobs/{id}', {
			body: { state: 'cancelled', progress: { phase: 'reading', done: 0, total: 0 } }
		});
		const onclose = vi.fn();
		render(RevealNameDialog, { props: { hash: HASH, open: true, onclose } });
		await screen.findByRole('dialog');
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(onclose).toHaveBeenCalledTimes(1);
	});

	it('Block this drops the picked words when the torrent changes', async () => {
		net.on('GET /search', { body: { total: 1, total_approx: false, hits: [] } });
		const r = render(BlockThisDialog, {
			props: { torrent: { hash: HASH, hash_v2: null, name: 'A B C' }, open: true }
		});
		await screen.findByRole('dialog');
		await fireEvent.click(screen.getByRole('radio', { name: 'A phrase from the name' }));
		await fireEvent.click(screen.getAllByTestId('block-word')[1]!);
		expect(screen.getByTestId('block-query').textContent).toBe('name:"B"');
		await r.rerender({ torrent: { hash: HASH, hash_v2: null, name: 'X Y Z' }, open: true });
		await waitFor(() => expect(screen.queryByTestId('block-query')).toBeNull());
		expect(screen.getAllByTestId('block-word').map((w) => w.getAttribute('aria-pressed'))).toEqual([
			'false',
			'false',
			'false'
		]);
	});
});
