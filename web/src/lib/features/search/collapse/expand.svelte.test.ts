// "+N other versions" opened in place: the list's rows under an open group, and → / ←.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { hit } from '../results/fixtures';
import type { SearchHit } from '../results/ResultRow.svelte';

vi.setConfig({ testTimeout: 20_000 });
const WAIT = { timeout: 10_000 };

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));

const { SHOWN, createExpand, hitOf, isGroupRow, keyOf } = await import('./expand.svelte');

const group = (hits: SearchHit[], truncated = false) => ({
	other_versions: { hits, reason: null, status: 'available', truncated }
});
const kinds = (rows: ReturnType<ReturnType<typeof createExpand>['rows']>) =>
	rows.map((r) => (isGroupRow(r) ? `${r.row}:${hitOf(r).id}` : `hit:${r.id}`));
const key = (k: string, mods: Partial<KeyboardEventInit> = {}) =>
	new KeyboardEvent('keydown', { key: k, cancelable: true, ...mods });

beforeEach(() => {
	get.mockReset();
});

describe('createExpand', () => {
	it('leaves the hits as they are while no group is open', () => {
		const hits = [hit(1, { others: 2 }), hit(2)];
		expect(createExpand().rows(hits)).toBe(hits);
	});

	it('opens a group under its best row: loading, then up to five versions and Show all', async () => {
		let answer!: (v: unknown) => void;
		get.mockImplementation(() => new Promise((r) => (answer = r)));
		const best = hit(1, { others: 7 });
		const hits = [best, hit(2)];
		const x = createExpand();
		x.expand(best);
		expect(kinds(x.rows(hits))).toEqual(['hit:1', 'loading:1', 'hit:2']);
		expect(get).toHaveBeenCalledWith('/torrents/{hash}/siblings', {
			path: { hash: best.hash },
			signal: expect.any(AbortSignal)
		});
		// The best row itself is never listed under itself.
		answer(group([best, ...[11, 12, 13, 14, 15, 16, 17].map((n) => hit(n))]));
		await vi.waitFor(() => expect(x.rows(hits)).toHaveLength(SHOWN + 3), WAIT);
		expect(kinds(x.rows(hits))).toEqual([
			'hit:1',
			'version:11',
			'version:12',
			'version:13',
			'version:14',
			'version:15',
			'more:1',
			'hit:2'
		]);
		expect(x.isOpen(best.hash)).toBe(true);
		// Keys carry the group, so a version's never meets another row's.
		expect(keyOf(x.rows(hits)[1]!)).toBe(`group:${best.hash}:${hit(11).hash}`);
	});

	it('shows Show all only when there is more than the rows in place', async () => {
		get.mockResolvedValue(group([hit(11), hit(12)]));
		const best = hit(1, { others: 2 });
		const x = createExpand();
		x.expand(best);
		await vi.waitFor(() => expect(x.rows([best])).toHaveLength(3), WAIT);
		expect(kinds(x.rows([best]))).toEqual(['hit:1', 'version:11', 'version:12']);
		// A truncated list has more on the server.
		get.mockResolvedValue(group([hit(11)], true));
		const other = hit(2, { others: 1 });
		x.expand(other);
		await vi.waitFor(() => expect(kinds(x.rows([other]))).toContain('more:2'), WAIT);
	});

	it('says when nothing came back, and retries a failed load', async () => {
		get.mockResolvedValueOnce(group([]));
		const a = hit(1, { others: 1 });
		const x = createExpand();
		x.expand(a);
		await vi.waitFor(() => expect(kinds(x.rows([a]))).toEqual(['hit:1', 'none:1']), WAIT);

		get.mockRejectedValueOnce(new Error('down'));
		const b = hit(2, { others: 1 });
		x.expand(b);
		await vi.waitFor(() => expect(kinds(x.rows([b]))).toEqual(['hit:2', 'failed:2']), WAIT);
		get.mockResolvedValueOnce(group([hit(21)]));
		x.expand(b); // a failed group is fetched again
		await vi.waitFor(() => expect(kinds(x.rows([b]))).toEqual(['hit:2', 'version:21']), WAIT);
		x.expand(b); // an open one is not
		expect(get).toHaveBeenCalledTimes(3);
	});

	it('never opens a row without other versions', () => {
		const x = createExpand();
		x.expand(hit(1));
		x.expand(hit(2, { others: 0 }));
		expect(get).not.toHaveBeenCalled();
	});

	it('closes a group, cancelling its load, and toggles', async () => {
		let signal!: AbortSignal;
		get.mockImplementation((_p, o: { signal: AbortSignal }) => {
			signal = o.signal;
			return new Promise(() => {});
		});
		const a = hit(1, { others: 3 });
		const x = createExpand();
		x.toggle(a);
		expect(x.isOpen(a.hash)).toBe(true);
		x.toggle(a);
		expect(x.isOpen(a.hash)).toBe(false);
		expect(signal.aborted).toBe(true);
		expect(x.rows([a])).toEqual([a]);
	});

	it('closes every group when the query changes', async () => {
		get.mockResolvedValue(group([hit(11)]));
		const a = hit(1, { others: 1 });
		const x = createExpand();
		x.follow('q1');
		x.expand(a);
		await vi.waitFor(() => expect(x.rows([a])).toHaveLength(2), WAIT);
		x.follow('q1'); // the same query (a new page) keeps them
		expect(x.isOpen(a.hash)).toBe(true);
		x.follow('q2');
		expect(x.isOpen(a.hash)).toBe(false);
	});

	it('ignores an answer for a group closed while it was out', async () => {
		let answer!: (v: unknown) => void;
		get.mockImplementation(() => new Promise((r) => (answer = r)));
		const a = hit(1, { others: 1 });
		const x = createExpand();
		x.expand(a);
		x.close(a.hash);
		answer(group([hit(11)]));
		await Promise.resolve();
		await Promise.resolve();
		expect(x.isOpen(a.hash)).toBe(false);
	});

	describe('keys', () => {
		it('→ opens the focused group and ← closes it', async () => {
			get.mockResolvedValue(group([hit(11)]));
			const a = hit(1, { others: 1 });
			const x = createExpand();
			const right = key('ArrowRight');
			expect(x.key(right, a)).toBeUndefined();
			expect(right.defaultPrevented).toBe(true);
			expect(x.isOpen(a.hash)).toBe(true);
			const left = key('ArrowLeft');
			expect(x.key(left, a)).toBeUndefined();
			expect(left.defaultPrevented).toBe(true);
			expect(x.isOpen(a.hash)).toBe(false);
		});

		it('← on a version closes its group and sends focus to the best row', async () => {
			get.mockResolvedValue(group([hit(11)]));
			const a = hit(1, { others: 1 });
			const x = createExpand();
			x.expand(a);
			await vi.waitFor(() => expect(x.rows([a])).toHaveLength(2), WAIT);
			const version = x.rows([a])[1]!;
			expect(x.key(key('ArrowLeft'), version)).toBe(a.hash);
			expect(x.isOpen(a.hash)).toBe(false);
		});

		it('leaves other keys, modified arrows and rows without versions to the list', () => {
			const x = createExpand();
			for (const e of [
				key('ArrowRight', { shiftKey: true }),
				key('ArrowDown'),
				key('ArrowLeft') // nothing open
			]) {
				x.key(e, hit(1, { others: 2 }));
				expect(e.defaultPrevented).toBe(false);
			}
			const plain = key('ArrowRight');
			x.key(plain, hit(2));
			expect(plain.defaultPrevented).toBe(false);
			expect(get).not.toHaveBeenCalled();
		});
	});
});

describe('settledText', () => {
	it('says how many versions show, none, or that they failed', async () => {
		const { settledText } = await import('./expand.svelte');
		expect(settledText(3)).toBe('3 other versions shown');
		expect(settledText(1)).toBe('1 other version shown');
		expect(settledText(0)).toBe('No other versions are available now.');
		expect(settledText(null)).toBe("Other versions couldn't load.");
	});
});
