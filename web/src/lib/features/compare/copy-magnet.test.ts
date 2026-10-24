import { afterEach, describe, expect, it, vi } from 'vitest';
import { copyShortcut, copyText } from './copy-magnet';

const setClipboard = (value: unknown) =>
	Object.defineProperty(navigator, 'clipboard', { value, configurable: true });
const setExec = (fn: unknown) =>
	Object.defineProperty(document, 'execCommand', { value: fn, configurable: true });

afterEach(() => {
	setClipboard(undefined);
	setExec(undefined);
});

describe('copyText', () => {
	it('uses the Clipboard API when there is one', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		setClipboard({ writeText });
		expect(await copyText('magnet:?xt=a')).toBe(true);
		expect(writeText).toHaveBeenCalledWith('magnet:?xt=a');
	});

	it('falls back to a hidden textarea where the API is missing (plain HTTP)', async () => {
		const exec = vi.fn((cmd: string) => {
			expect(cmd).toBe('copy');
			expect(document.querySelector('textarea')?.value).toBe('magnet:?xt=b');
			return true;
		});
		setExec(exec);
		expect(await copyText('magnet:?xt=b')).toBe(true);
		expect(exec).toHaveBeenCalled();
		expect(document.querySelector('textarea')).toBeNull();
	});

	it('falls back when the API rejects, and reports failure when both fail', async () => {
		setClipboard({ writeText: vi.fn().mockRejectedValue(new Error('denied')) });
		setExec(vi.fn(() => false));
		expect(await copyText('x')).toBe(false);
		setExec(
			vi.fn(() => {
				throw new Error('no');
			})
		);
		expect(await copyText('x')).toBe(false);
		expect(document.querySelector('textarea')).toBeNull();
	});
});

describe('copyShortcut', () => {
	it('names the key for the platform', () => {
		const platform = vi.spyOn(navigator, 'platform', 'get');
		platform.mockReturnValue('MacIntel');
		expect(copyShortcut()).toBe('⌘C');
		platform.mockReturnValue('Win32');
		expect(copyShortcut()).toBe('Ctrl+C');
	});
});
