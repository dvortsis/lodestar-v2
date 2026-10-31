// Settings → Logging part 2 (S-logs-2b) against a stubbed fetch: the log files with their badges and
// downloads (torrent details removed by default), the folder and the Docker hint, the Danger zone's
// dialogs (count and size, Cancel sends nothing, files left behind are named), the console detail's
// question, and the preview of the staged settings.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { dismissAll } from '../form/dialogs';
import type { SettingsForm } from '../form';
import LogFiles from './LogFiles.svelte';
import Preview from './Preview.svelte';
import { LoggingCatalog } from './catalog.svelte';
import { confirmConsoleDetail } from './outputs';

vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

const FILES = [
	{
		name: 'lodestar.jsonl',
		size: 2048,
		details: true,
		opt_in: false,
		current: true,
		compressed: false,
		from: '2026-10-31T10:00:00Z',
		to: '2026-10-31T11:00:00Z'
	},
	{
		name: 'lodestar-1.jsonl.gz',
		size: 1024,
		details: false,
		opt_in: false,
		current: false,
		compressed: true,
		from: null,
		to: null
	}
];
let calls: { method: string; path: string; search: string; body: unknown }[];
let deleteReply: unknown;
let listReply: { status: number; body: unknown };
let previewReply: unknown;

beforeEach(() => {
	initLocale('en');
	calls = [];
	deleteReply = { deleted: 1, bytes: 2048, skipped: [] };
	previewReply = { sections: { logging: { revision: 1, fields: {} } } };
	listReply = {
		status: 200,
		body: { files: FILES, folder: '/data/logs', index_readable: true, reason: null, state: 'on' }
	};
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const u = new URL(url, 'http://x');
			const path = u.pathname.replace('/api/v1', '');
			const method = init.method ?? 'GET';
			calls.push({
				method,
				path,
				search: u.search,
				body: init.body ? JSON.parse(String(init.body)) : undefined
			});
			const reply = (b: unknown) =>
				new Response(JSON.stringify(b), { headers: { 'Content-Type': 'application/json' } });
			if (path === '/logs/files' && method === 'GET')
				return new Response(JSON.stringify(listReply.body), {
					status: listReply.status,
					headers: { 'Content-Type': 'application/json' }
				});
			if (path === '/logs/files' && method === 'DELETE' && deleteReply === 'fail')
				return new Response(JSON.stringify({ error: { code: 'internal', message: 'disk' } }), {
					status: 500,
					headers: { 'Content-Type': 'application/json' }
				});
			if (path === '/logs/files' && method === 'DELETE') return reply(deleteReply);
			if (path === '/settings/preview')
				return previewReply === 'fail' ? new Response('{}', { status: 500 }) : reply(previewReply);
			return new Response('{}', { status: 404 });
		})
	);
});
afterEach(() => {
	dismissAll();
	vi.unstubAllGlobals();
});

const hrefOf = (name: string) =>
	screen.getByRole('link', { name: `Download ${name}` }).getAttribute('href');

describe('log files', () => {
	it('without torrent details: no Remove box, and the zip has no redact; no Docker hint', async () => {
		listReply.body = {
			files: [FILES[1]],
			folder: '/data/logs',
			index_readable: true,
			reason: null,
			state: 'on'
		};
		const catalog = new LoggingCatalog();
		catalog.deployment = 'tarball';
		render(LogFiles, { catalog });
		await screen.findByTestId('log-file-lodestar-1.jsonl.gz');
		expect(screen.queryByRole('checkbox', { name: 'Remove torrent details' })).toBeNull();
		expect(screen.getByTestId('log-download-all').getAttribute('href')).toBe(
			'/api/v1/logs/archive.zip?format=text'
		);
		expect(screen.queryByTestId('log-files-docker')).toBeNull();
		expect(screen.getByTestId('log-file-time-lodestar-1.jsonl.gz').textContent).toBe('— – —');
	});

	it('says why nothing is listed, and when the details index is unreadable', async () => {
		listReply.body = {
			files: [],
			folder: null,
			index_readable: false,
			reason: 'not_private',
			state: 'unavailable'
		};
		render(LogFiles, { catalog: new LoggingCatalog() });
		expect((await screen.findByTestId('log-files-state')).textContent).toContain(
			'other users of this machine'
		);
		expect(screen.getByTestId('log-files-index')).toBeTruthy();
		expect(screen.getByText('No log files yet.')).toBeTruthy();
	});

	it('says so when the list fails', async () => {
		listReply = { status: 500, body: { error: { code: 'internal', message: 'x' } } };
		render(LogFiles, { catalog: new LoggingCatalog() });
		expect((await screen.findByRole('alert')).textContent).toContain("couldn't be listed");
	});

	it('copies the folder, or selects it to copy by hand', async () => {
		// jsdom isn't a secure context, so the clipboard falls back to selecting the path.
		render(LogFiles, { catalog: new LoggingCatalog() });
		await fireEvent.click(await screen.findByRole('button', { name: 'Copy the folder path' }));
		await until(() => expect(getSelection()?.toString()).toBe('/data/logs'));
	});

	it('lists the files, badges those with details, and downloads them redacted by default', async () => {
		const catalog = new LoggingCatalog();
		catalog.deployment = 'docker';
		render(LogFiles, { catalog });
		await screen.findByTestId('log-file-lodestar.jsonl');
		expect(screen.getByTestId('log-file-lodestar.jsonl').textContent).toContain(
			'Contains torrent details'
		);
		expect(screen.getByTestId('log-file-lodestar-1.jsonl.gz').textContent).not.toContain(
			'Contains torrent details'
		);
		expect(hrefOf('lodestar.jsonl')).toBe(
			'/api/v1/logs/files/lodestar.jsonl?format=text&redact=true'
		);
		expect(hrefOf('lodestar-1.jsonl.gz')).toBe(
			'/api/v1/logs/files/lodestar-1.jsonl.gz?format=text'
		);
		expect(screen.getByTestId('log-download-all').getAttribute('href')).toBe(
			'/api/v1/logs/archive.zip?format=text&redact=true'
		);
		expect(screen.getByTestId('log-folder').textContent).toBe('/data/logs');
		expect(screen.getByText(/docker logs/)).toBeTruthy();
		// Unticking Remove torrent details is the user's own choice, for the downloads made now.
		await fireEvent.click(screen.getByRole('checkbox', { name: 'Remove torrent details' }));
		await fireEvent.click(screen.getByRole('radio', { name: 'JSON lines' }));
		await until(() =>
			expect(hrefOf('lodestar.jsonl')).toBe(
				'/api/v1/logs/files/lodestar.jsonl?format=jsonl&redact=false'
			)
		);
	});

	it('deletes only after its dialog, which names the count and size; Cancel sends nothing', async () => {
		render(LogFiles, { catalog: new LoggingCatalog() });
		await screen.findByTestId('log-file-lodestar.jsonl');
		await fireEvent.click(
			screen.getByRole('button', { name: 'Delete files with torrent details…' })
		);
		const dialog = await screen.findByRole(
			'dialog',
			{ name: 'Delete the log files with torrent details?' },
			{ timeout: 10_000 }
		);
		expect(dialog.textContent).toMatch(/Log files to delete: 1 \(2\.00\sKiB\)/);
		expect(document.activeElement?.textContent?.trim()).toBe('Cancel');
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		await until(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(calls.some((c) => c.method === 'DELETE')).toBe(false);

		deleteReply = {
			deleted: 1,
			bytes: 1024,
			skipped: [{ name: 'lodestar.jsonl', reason: 'not_a_regular_file' }]
		};
		await fireEvent.click(screen.getByRole('button', { name: 'Delete all log files…' }));
		await fireEvent.click(
			await screen.findByRole(
				'button',
				{ name: /Delete log files \(2, 3\.00\sKiB\)/ },
				{ timeout: 10_000 }
			)
		);
		await until(() =>
			expect(calls.find((c) => c.method === 'DELETE')?.body).toEqual({ which: 'all' })
		);
		await until(() => expect(screen.getByRole('alert').textContent).toContain('lodestar.jsonl'));
		expect(screen.getByText(/Log files deleted: 1 \(1\.00\sKiB\)\./)).toBeTruthy();
	});

	it('says so when a delete fails, and the list is read again', async () => {
		render(LogFiles, { catalog: new LoggingCatalog() });
		await screen.findByTestId('log-file-lodestar.jsonl');
		deleteReply = 'fail';
		await fireEvent.click(screen.getByRole('button', { name: 'Delete all log files…' }));
		await fireEvent.click(
			await screen.findByRole('button', { name: /Delete log files \(2,/ }, { timeout: 10_000 })
		);
		await until(() =>
			expect(screen.getByRole('alert').textContent).toContain("Couldn't delete the files")
		);
		expect(calls.filter((c) => c.path === '/logs/files' && c.method === 'GET')).toHaveLength(2);
	});

	it('disables deleting when the list read after a delete fails, so no dialog shows a stale count', async () => {
		render(LogFiles, { catalog: new LoggingCatalog() });
		await screen.findByTestId('log-file-lodestar.jsonl');
		await fireEvent.click(screen.getByRole('button', { name: 'Delete all log files…' }));
		const confirm = await screen.findByRole(
			'button',
			{ name: /Delete log files \(2,/ },
			{ timeout: 10_000 }
		);
		listReply = { status: 500, body: { error: { code: 'internal', message: 'x' } } };
		await fireEvent.click(confirm);
		await until(() =>
			expect(screen.getByText(/Log files deleted: 1 \(2\.00\sKiB\)\./)).toBeTruthy()
		);
		await until(() =>
			expect(screen.getByRole('alert').textContent).toContain("couldn't be listed")
		);
		const del = (name: string) =>
			(screen.getByRole('button', { name }) as HTMLButtonElement).disabled;
		expect(del('Delete all log files…')).toBe(true);
		expect(del('Delete files with torrent details…')).toBe(true);
		expect(screen.queryByTestId('log-file-lodestar.jsonl')).toBeNull();
	});
});

const fakeForm = (detail: { value: unknown; dirty: boolean }) =>
	({
		specs: [
			{ key: 'logging.verbosity', name: 'verbosity', preview: true, group: 'events' },
			{ key: 'logging.file_size_mib', name: 'file_size_mib', preview: true, group: 'file' }
		],
		field: (key: string) =>
			key === 'logging.console_detail'
				? detail
				: key === 'logging.file_size_mib'
					? { value: 32, dirty: true, locked: false }
					: { value: 'normal', dirty: false, locked: false }
	}) as unknown as SettingsForm;

describe('outputs', () => {
	it("the preview says when it isn't available", async () => {
		previewReply = 'fail';
		render(Preview, { form: fakeForm({ value: 'private', dirty: false }) });
		await screen.findByText(/preview isn't available/, undefined, { timeout: 10_000 });
	});

	it('asks before the console gets torrent details; Cancel refuses the save', async () => {
		expect(await confirmConsoleDetail(fakeForm({ value: 'private', dirty: true }))).toBe(true);
		expect(await confirmConsoleDetail(fakeForm({ value: 'file', dirty: false }))).toBe(true);
		const asked = confirmConsoleDetail(fakeForm({ value: 'file', dirty: true }));
		await screen.findByRole(
			'dialog',
			{ name: 'Show torrent details in the console?' },
			{ timeout: 10_000 }
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		expect(await asked).toBe(false);
	});

	it('previews the staged settings: volume per day, days kept and example lines', async () => {
		previewReply = {
			sections: {
				logging: {
					revision: 1,
					fields: {
						verbosity: {
							restart_required: false,
							value: 'normal',
							warnings: [],
							estimates: [
								{ name: 'console_mib_per_day', value: 1.5 },
								{ name: 'file_mib_per_day', value: 12 },
								{ name: 'file_days_kept', value: 21 },
								{
									name: 'examples',
									value: [{ event: 'fetch.failed', console: null, file: '{"msg":"ref:7f3a2c9e"}' }]
								}
							]
						}
					}
				}
			}
		};
		render(Preview, { form: fakeForm({ value: 'private', dirty: false }) });
		await until(() =>
			expect(screen.getByTestId('log-volume-file').textContent).toMatch(/12\.0\sMiB/)
		);
		expect(screen.getByTestId('log-volume-file').textContent).toContain('21 days');
		expect(screen.getByTestId('log-volume-console').textContent).toMatch(/1\.50\sMiB/);
		expect(screen.getByTestId('log-examples').textContent).toContain('ref:7f3a2c9e');
		expect(calls.find((c) => c.path === '/settings/preview')?.body).toEqual({
			logging: { file_size_mib: 32 }
		});
	});
});
