import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import SettingsExport from './SettingsExport.svelte';

const request = vi.hoisted(() => vi.fn());
vi.mock('./export', () => ({ requestSettingsExport: request }));
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
vi.setConfig({ testTimeout: 15_000 });
const create = vi.fn(() => 'blob:settings-fixture');
const revoke = vi.fn();
let click: ReturnType<typeof vi.spyOn>;
beforeEach(() => {
	request.mockReset();
	create.mockClear();
	revoke.mockClear();
	vi.stubGlobal(
		'URL',
		class extends URL {
			static createObjectURL = create;
			static revokeObjectURL = revoke;
		}
	);
	click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
});
afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

it('loads only on request and releases its retained file on unmount', async () => {
	request.mockResolvedValue(new Blob(['fixture']));
	const view = render(SettingsExport);
	expect(request).not.toHaveBeenCalled();
	await fireEvent.click(screen.getByRole('button', { name: 'Download saved settings' }));
	await until(() => expect(click).toHaveBeenCalledTimes(1));
	expect(request.mock.calls[0]?.[0]).toBe(false);
	expect(click.mock.instances[0]).toMatchObject({ download: 'lodestar-settings.toml' });
	await fireEvent.click(screen.getByRole('switch', { name: 'Include interface preferences' }));
	await fireEvent.click(screen.getByRole('button', { name: 'Download saved settings' }));
	await until(() => expect(click).toHaveBeenCalledTimes(2));
	expect(request.mock.calls[1]?.[0]).toBe(true);
	expect(revoke).toHaveBeenCalledTimes(1);
	view.unmount();
	expect(revoke).toHaveBeenCalledTimes(2);
});

it('cancels without downloading a late file and restores focus', async () => {
	let finish!: (value: Blob) => void;
	request.mockImplementation(
		() =>
			new Promise<Blob>((resolve) => {
				finish = resolve;
			})
	);
	render(SettingsExport);
	const button = screen.getByRole('button', { name: 'Download saved settings' });
	await fireEvent.click(button);
	await fireEvent.click(button);
	expect(request).toHaveBeenCalledTimes(1);
	await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
	expect(request.mock.calls[0]?.[1].aborted).toBe(true);
	finish(new Blob(['late']));
	await until(() => expect(document.activeElement).toBe(button));
	expect(screen.getByRole('status').textContent).toContain('cancelled');
	expect(create).not.toHaveBeenCalled();
});

it('aborts on unmount and ignores a late response', async () => {
	let finish!: (value: Blob) => void;
	request.mockImplementation(
		() =>
			new Promise<Blob>((resolve) => {
				finish = resolve;
			})
	);
	const view = render(SettingsExport);
	await fireEvent.click(screen.getByRole('button', { name: 'Download saved settings' }));
	view.unmount();
	expect(request.mock.calls[0]?.[1].aborted).toBe(true);
	finish(new Blob(['late']));
	await Promise.resolve();
	expect(create).not.toHaveBeenCalled();
});

it('a cancelled request cannot replace a later successful download', async () => {
	let finish!: (value: Blob) => void;
	const first = new Promise<Blob>((resolve) => {
		finish = resolve;
	});
	request.mockReturnValueOnce(first).mockResolvedValueOnce(new Blob(['new file']));
	render(SettingsExport);
	const button = screen.getByRole('button', { name: 'Download saved settings' });
	await fireEvent.click(button);
	await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
	await fireEvent.click(button);
	await until(() => expect(click).toHaveBeenCalledTimes(1));
	finish(new Blob(['old file']));
	await first;
	expect(create).toHaveBeenCalledTimes(1);
	expect(screen.getByRole('status').textContent).toContain('Download started');
});

it('shows a safe error and allows explicit retry', async () => {
	request
		.mockRejectedValueOnce(new Error('private-path-or-response'))
		.mockResolvedValueOnce(new Blob(['fixture']));
	render(SettingsExport);
	const button = screen.getByRole('button', { name: 'Download saved settings' });
	await fireEvent.click(button);
	await until(() => expect(screen.getByRole('status').textContent).toContain('Could not download'));
	expect(document.body.textContent).not.toContain('private-path-or-response');
	expect(create).not.toHaveBeenCalled();
	await fireEvent.click(button);
	await until(() => expect(click).toHaveBeenCalledTimes(1));
});
