import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import { stubLayout } from '$lib/ui/lists/testing/layout';
import { torrentDetail } from '../fixtures';
import { createTreeBuilder, type WorkerRequest } from './tree.worker';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));

const { default: FileTree } = await import('./FileTree.svelte');

const files = (n: number, from = 0): Array<{ path: string; size: number; attrs: string[] }> =>
	Array.from({ length: n }, (_, i) => ({
		path: `dir${(from + i) % 5}/file${from + i}.bin`,
		size: 10,
		attrs: []
	}));
const detail = (total: number, held = 500) =>
	torrentDetail({ file_list: files(Math.min(held, total)), file_list_total: total, files: total });

/** A Worker that runs the real handler on a microtask, recording how it was made. */
class FakeWorker {
	static made: Array<{ url: string; options: WorkerOptions | undefined; worker: FakeWorker }> = [];
	onmessage: ((e: MessageEvent) => void) | null = null;
	terminated = false;
	private builder = createTreeBuilder();
	constructor(url: URL | string, options?: WorkerOptions) {
		FakeWorker.made.push({ url: String(url), options, worker: this });
	}
	postMessage(message: WorkerRequest) {
		const out = this.builder.handle(message);
		if (out)
			queueMicrotask(
				() => !this.terminated && this.onmessage?.({ data: out.reply } as MessageEvent)
			);
	}
	terminate() {
		this.terminated = true;
	}
}

beforeAll(() => stubLayout());
beforeEach(() => {
	get.mockReset();
	FakeWorker.made = [];
	vi.stubGlobal('Worker', FakeWorker);
});
afterEach(() => vi.unstubAllGlobals());

const rows = () => screen.queryAllByRole('treeitem');
const root = () => screen.getByTestId('detail-file-tree');

describe('FileTree', () => {
	it('builds on the main thread for a small torrent and pages nothing', async () => {
		render(FileTree, { detail: torrentDetail() });
		expect(root().dataset.treeWorker).toBe('false');
		expect(FakeWorker.made).toHaveLength(0);
		await waitFor(() => expect(rows().length).toBeGreaterThan(0));
		expect(rows()[0]!.textContent).toContain('Show');
		expect(get).not.toHaveBeenCalled();
		expect(root().getAttribute('aria-busy')).toBe('false');
	});

	it('pages the rest after the first 500 and says so while it does', async () => {
		let release!: () => void;
		get.mockImplementation(
			() =>
				new Promise((resolve) => {
					release = () => resolve({ files: files(700, 500), offset: 500, total: 1200 });
				})
		);
		render(FileTree, { detail: detail(1200) });
		await waitFor(() => expect(get).toHaveBeenCalledTimes(1));
		expect(get.mock.calls[0]![1].query).toEqual({ offset: 500, limit: 1000 });
		expect(get.mock.calls[0]![1].path).toEqual({ hash: torrentDetail().hash });
		expect(root().getAttribute('aria-busy')).toBe('true');
		expect(screen.getByTestId('detail-file-tree-loading').textContent).toBe(
			'Loading all 1,200 files…'
		);
		release();
		await waitFor(() => expect(root().getAttribute('aria-busy')).toBe('false'));
		expect(screen.queryByTestId('detail-file-tree-loading')).toBeNull();
	});

	it('keeps what loaded and offers Retry when a page fails, then continues from there', async () => {
		get.mockRejectedValueOnce(new ApiError(503, 'unavailable', 'x'));
		render(FileTree, { detail: detail(1200) });
		const alert = await screen.findByRole('alert');
		expect(alert.textContent).toContain("Couldn't load the rest of the files.");
		expect(rows().length).toBeGreaterThan(0); // the first 500 stay
		get.mockResolvedValueOnce({ files: files(700, 500), offset: 500, total: 1200 });
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
		expect(get).toHaveBeenCalledTimes(2);
		expect(get.mock.calls[1]![1].query.offset).toBe(500);
	});

	it('builds in a module worker above 2,000 files, then terminates it on unmount', async () => {
		get.mockImplementation(async (_p: string, o: { query: { offset: number } }) => ({
			files: files(Math.min(1000, 3000 - o.query.offset), o.query.offset),
			offset: o.query.offset,
			total: 3000
		}));
		const { unmount } = render(FileTree, { detail: detail(3000) });
		expect(root().dataset.treeWorker).toBe('true');
		expect(FakeWorker.made).toHaveLength(1);
		const { url, options, worker } = FakeWorker.made[0]!;
		expect(options).toEqual({ type: 'module' });
		expect(url).toMatch(/tree\.worker/);
		expect(url.startsWith('blob:')).toBe(false);
		await waitFor(() => expect(rows().length).toBeGreaterThan(0));
		await waitFor(() => expect(get).toHaveBeenCalledTimes(3)); // offsets 500, 1500, 2500
		expect(get.mock.calls.map((c) => c[1].query.offset).sort((a, b) => a - b)).toEqual([
			500, 1500, 2500
		]);
		unmount();
		expect(worker.terminated).toBe(true);
	});

	it('uses the worker exactly above the threshold', () => {
		get.mockResolvedValue({ files: [], offset: 500, total: 2000 });
		const a = render(FileTree, { detail: detail(2000, 500) });
		expect(root().dataset.treeWorker).toBe('false');
		a.unmount();
		render(FileTree, { detail: detail(2001, 500) });
		expect(root().dataset.treeWorker).toBe('true');
	});

	it('filters in the worker after a 120 ms debounce, and keeps ancestors of hits', async () => {
		render(FileTree, { detail: detail(3000, 3000) });
		await waitFor(() => expect(rows().length).toBeGreaterThan(0));
		const input = screen.getByRole('searchbox', { name: 'Filter files' });
		await fireEvent.input(input, { target: { value: 'file1234.' } });
		// the debounce has not fired yet
		expect(rows().length).toBeGreaterThan(1);
		await waitFor(() => expect(rows().length).toBe(2), { timeout: 2000 });
		expect(rows()[0]!.textContent).toContain('dir4');
		expect(rows()[1]!.textContent).toContain('file1234.bin');
	});

	it('hides padding files unless asked', async () => {
		const padded = torrentDetail({
			file_list: [
				{ path: 'a.mkv', size: 10, attrs: [] },
				{ path: '.pad/1', size: 5, attrs: ['padding'] }
			],
			file_list_total: 2,
			files: 2
		});
		render(FileTree, { detail: padded });
		await waitFor(() => expect(rows()).toHaveLength(1));
		await fireEvent.click(screen.getByRole('checkbox', { name: 'Show padding files' }));
		// the padding folder appears (collapsed) next to the file
		await waitFor(() => expect(rows()).toHaveLength(2));
		expect(
			rows()
				.map((r) => r.textContent)
				.join()
		).toContain('.pad');
	});

	it('marks the files a search matched and opens their folders', async () => {
		render(FileTree, { detail: torrentDetail(), matched: [1] });
		await waitFor(() => expect(rows().length).toBeGreaterThan(1));
		const marked = rows().filter((r) => r.textContent!.includes('Matched'));
		expect(marked).toHaveLength(1);
		expect(marked[0]!.textContent).toContain('S01E02.mkv');
	});

	it('starts over for another torrent', async () => {
		const { rerender } = render(FileTree, { detail: torrentDetail() });
		await waitFor(() => expect(rows().length).toBeGreaterThan(0));
		rerender({
			detail: torrentDetail({
				hash: 'f'.repeat(40),
				file_list: [{ path: 'other.txt', size: 1, attrs: [] }],
				file_list_total: 1
			})
		});
		await waitFor(() => expect(rows()).toHaveLength(1));
		expect(rows()[0]!.textContent).toContain('other.txt');
	});
});
