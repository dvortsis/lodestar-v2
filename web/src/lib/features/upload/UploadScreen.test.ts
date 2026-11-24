// The upload part of Settings → Add torrent files: limits named before sending, one result per
// file, Retry only where the server or the connection failed, and nothing sent after leaving.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, configure, fireEvent, render, screen } from '@testing-library/svelte';

vi.mock('./WatchFolder.svelte', async () => ({
	default: (await import('../settings/shell/testing/Stub.svelte')).default
}));

import UploadScreen from './UploadScreen.svelte';
import { MAX_BYTES, MAX_FILES } from './upload';

configure({ asyncUtilTimeout: 10_000 });
const wait = (check: () => void) => vi.waitFor(check, { timeout: 10_000 });

const json = (status: number, body: unknown, headers: Record<string, string> = {}) =>
	new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json', ...headers }
	});
const refusal = (status: number, code: string, headers: Record<string, string> = {}) =>
	json(status, { error: { code, message: 'server text' } }, headers);
const torrent = (name: string, size = 64) => new File([new Uint8Array(size)], name);
const big = (name: string) => {
	const f = torrent(name, 1);
	Object.defineProperty(f, 'size', { value: MAX_BYTES + 1 });
	return f;
};

let fetch: ReturnType<typeof vi.fn>;
beforeEach(() => {
	fetch = vi.fn();
	vi.stubGlobal('fetch', fetch);
});
afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

const input = () => screen.getByTestId('upload-input') as HTMLInputElement;
async function pick(...files: File[]) {
	await fireEvent.change(input(), { target: { files } });
}
const rows = () => screen.queryAllByTestId('upload-item');
const states = () => rows().map((r) => r.getAttribute('data-state'));

describe('UploadScreen', () => {
	it('names the limits before anything is sent, and Choose files opens the picker', async () => {
		render(UploadScreen);
		expect(screen.getByRole('heading', { level: 1, name: 'Add torrent files' })).toBeTruthy();
		expect(screen.getByTestId('upload-limits').textContent).toContain(
			`Up to ${MAX_FILES} files at a time, each at most 8.00\u00a0MiB`
		);
		const click = vi.spyOn(input(), 'click');
		await fireEvent.click(screen.getByRole('button', { name: 'Choose files' }));
		expect(click).toHaveBeenCalledOnce();
		expect(input().accept).toBe('.torrent,application/x-bittorrent');
		expect(input().multiple).toBe(true);
		expect(fetch).not.toHaveBeenCalled();
	});

	it('refuses a pick over the limit whole, naming it, and sends nothing', async () => {
		render(UploadScreen);
		await pick(...Array.from({ length: MAX_FILES + 1 }, (_, i) => torrent(`${i}.torrent`)));
		expect(screen.getByTestId('upload-too-many').textContent).toContain(
			`You chose ${MAX_FILES + 1} files, and Lodestar takes at most ${MAX_FILES} at a time`
		);
		expect(rows()).toHaveLength(0);
		expect(fetch).not.toHaveBeenCalled();
	});

	it('shows one result per file, in order, one request at a time', async () => {
		const hash = 'c'.repeat(40);
		let inFlight = 0;
		let most = 0;
		const answers = [
			json(202, { info_hash: hash, private: false }),
			refusal(400, 'malformed_torrent'),
			refusal(422, 'held'),
			refusal(422, 'refused'),
			json(202, { info_hash: 'd'.repeat(40), private: true })
		];
		fetch.mockImplementation(async () => {
			most = Math.max(most, ++inFlight);
			await Promise.resolve();
			inFlight--;
			return answers.shift()!;
		});
		render(UploadScreen);
		await pick(
			torrent('one.torrent'),
			torrent('two.torrent'),
			torrent('notes.txt'),
			big('huge.torrent'),
			torrent('three.torrent'),
			torrent('four.torrent'),
			torrent('five.torrent')
		);
		await wait(() =>
			expect(states()).toEqual([
				'added',
				'malformed',
				'malformed',
				'too_large',
				'held',
				'refused',
				'added'
			])
		);
		// The two refused before sending never reached the server.
		expect(fetch).toHaveBeenCalledTimes(5);
		expect(most).toBe(1);
		const text = rows().map(
			(r) => r.querySelector('[data-testid="upload-item-state"]')?.textContent
		);
		expect(text).toEqual([
			'Added',
			'Not a torrent file',
			'Not a torrent file',
			'Over 8.00 MiB, too large',
			'Held by filters',
			'Refused by the safety floor',
			'Added, private'
		]);
		const open = screen.getAllByTestId('upload-open');
		expect(open.map((a) => a.getAttribute('href'))).toEqual([`/t/${hash}`, `/t/${'d'.repeat(40)}`]);
		expect(open[0]!.getAttribute('aria-label')).toBe('Open one.torrent');
		expect(screen.getByTestId('upload-count-added').textContent).toBe('2 added');
		expect(screen.getByTestId('upload-count-malformed').textContent).toBe('2 not a torrent file');
		expect(screen.queryByTestId('upload-retry')).toBeNull();
		await wait(() =>
			expect(screen.getByTestId('upload-announce').textContent).toBe(
				'2 added, 2 not a torrent file, 1 held by filters, 1 refused by the safety floor, 1 too large'
			)
		);
	});

	it('a rate limit stops the rest; Retry sends them again', async () => {
		fetch
			.mockResolvedValueOnce(json(202, { info_hash: 'a'.repeat(40), private: false }))
			.mockResolvedValueOnce(refusal(429, 'rate_limited', { 'Retry-After': '5' }));
		render(UploadScreen);
		await pick(torrent('a.torrent'), torrent('b.torrent'), torrent('c.torrent'));
		await wait(() => expect(states()).toEqual(['added', 'rate_limited', 'stopped']));
		expect(fetch).toHaveBeenCalledTimes(2);
		expect(rows()[1]!.textContent).toContain('Too many uploads. Retry in 5 s');
		expect(screen.getByTestId('upload-count-failed').textContent).toBe('2 to retry');
		const retry = screen.getAllByTestId('upload-retry');
		expect(retry.map((b) => b.getAttribute('aria-label'))).toEqual([
			'Retry b.torrent',
			'Retry c.torrent'
		]);

		fetch.mockResolvedValueOnce(refusal(503, 'uploads_unavailable'));
		await fireEvent.click(retry[1]!);
		await wait(() => expect(states()).toEqual(['added', 'rate_limited', 'unavailable']));

		fetch.mockImplementation(async () => json(202, { info_hash: 'b'.repeat(40), private: false }));
		await fireEvent.click(screen.getByTestId('upload-retry-all'));
		await wait(() => expect(states()).toEqual(['added', 'added', 'added']));
		expect(fetch).toHaveBeenCalledTimes(5);
		expect(screen.queryByTestId('upload-retry-all')).toBeNull();
	});

	it('a network failure can be retried; Clear results empties the list', async () => {
		fetch.mockRejectedValueOnce(new TypeError('Failed to fetch'));
		render(UploadScreen);
		await pick(torrent('a.torrent'));
		await wait(() => expect(states()).toEqual(['failed']));
		expect(rows()[0]!.textContent).toContain("Couldn't send");
		await fireEvent.click(screen.getByTestId('upload-clear'));
		expect(rows()).toHaveLength(0);
	});

	it('a drop anywhere on the page adds the files', async () => {
		fetch.mockResolvedValue(json(202, { info_hash: 'e'.repeat(40), private: false }));
		render(UploadScreen);
		const files = [torrent('dropped.torrent')];
		const dataTransfer = { types: ['Files'], files, dropEffect: 'none' };
		await fireEvent.dragOver(window, { dataTransfer });
		expect(screen.getByTestId('upload-zone').dataset.dragging).toBe('true');
		await fireEvent.drop(window, { dataTransfer });
		await wait(() => expect(states()).toEqual(['added']));
		expect(screen.getByTestId('upload-zone').dataset.dragging).toBeUndefined();
		// A drag of text, not files, is left to the browser.
		await fireEvent.drop(window, { dataTransfer: { types: ['text/plain'], files: [] } });
		expect(rows()).toHaveLength(1);
	});

	it('leaving the screen sends nothing more', async () => {
		let release!: (r: Response) => void;
		fetch.mockImplementation(
			(_url: string, init: RequestInit) =>
				new Promise<Response>((resolve, reject) => {
					release = resolve;
					init.signal?.addEventListener('abort', () =>
						reject(new DOMException('Aborted', 'AbortError'))
					);
				})
		);
		const { unmount } = render(UploadScreen);
		await pick(torrent('a.torrent'), torrent('b.torrent'));
		await wait(() => expect(fetch).toHaveBeenCalledOnce());
		unmount();
		release(json(202, { info_hash: 'a'.repeat(40), private: false }));
		await Promise.resolve();
		expect(fetch).toHaveBeenCalledOnce();
	});
});
