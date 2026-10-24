import { afterEach, describe, expect, it, vi } from 'vitest';
import { copyText } from './clipboard';

const setSecure = (v: boolean) =>
	Object.defineProperty(window, 'isSecureContext', { value: v, configurable: true });
const setClipboard = (writeText?: (t: string) => Promise<void>) =>
	Object.defineProperty(navigator, 'clipboard', {
		value: writeText ? { writeText } : undefined,
		configurable: true
	});
const setExec = (impl: () => boolean) =>
	Object.defineProperty(document, 'execCommand', { value: vi.fn(impl), configurable: true });

afterEach(() => {
	setSecure(true);
	setClipboard(undefined);
});

describe('copyText', () => {
	it('uses the async clipboard in a secure context', async () => {
		const writeText = vi.fn(async () => {});
		setSecure(true);
		setClipboard(writeText);
		expect(await copyText('hello')).toBe('copied');
		expect(writeText).toHaveBeenCalledWith('hello');
	});

	it('falls back to execCommand when not secure', async () => {
		const writeText = vi.fn(async () => {});
		setSecure(false);
		setClipboard(writeText);
		setExec(() => true);
		expect(await copyText('x')).toBe('copied');
		expect(writeText).not.toHaveBeenCalled();
		expect(document.execCommand).toHaveBeenCalledWith('copy');
		expect(document.querySelector('textarea')).toBeNull();
	});

	it('falls back when the async clipboard throws', async () => {
		setSecure(true);
		setClipboard(async () => {
			throw new Error('denied');
		});
		setExec(() => true);
		expect(await copyText('x')).toBe('copied');
	});

	it('reports manual when execCommand fails or throws', async () => {
		setSecure(false);
		setExec(() => false);
		expect(await copyText('x')).toBe('manual');
		setExec(() => {
			throw new Error('nope');
		});
		expect(await copyText('x')).toBe('manual');
	});
});
