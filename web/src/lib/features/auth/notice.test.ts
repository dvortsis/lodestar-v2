// The notice waits for the UI font so it never reflows on screen, but never longer than the cap.
import { afterEach, expect, it, vi } from 'vitest';
import { onSessionOk } from '$lib/api/client';
import { FONT_WAIT_MS, fontsSettled, loadNotice, resetNoticeCache } from './notice';

afterEach(() => {
	vi.useRealTimers();
	delete (document as { fonts?: unknown }).fonts;
});

/** jsdom has no `document.fonts`; give it one with this `load`. */
const withFonts = (load: () => Promise<unknown>) =>
	Object.defineProperty(document, 'fonts', { value: { load }, configurable: true });

it('resolves at once without a font API', async () => {
	await expect(fontsSettled()).resolves.toBeUndefined();
});

it('waits for the UI font', async () => {
	let done!: () => void;
	const load = vi.fn(() => new Promise<unknown>((r) => (done = () => r([]))));
	withFonts(load);
	let settled = false;
	const p = fontsSettled().then(() => (settled = true));
	expect(load).toHaveBeenCalledWith('400 1em "Atkinson Hyperlegible Next"', 'Lodestar');
	await Promise.resolve();
	expect(settled).toBe(false);
	done();
	await p;
	expect(settled).toBe(true);
});

it('stops waiting after the cap when the font never arrives', async () => {
	vi.useFakeTimers();
	withFonts(() => new Promise(() => {}));
	const p = fontsSettled();
	await vi.advanceTimersByTimeAsync(FONT_WAIT_MS);
	await expect(p).resolves.toBeUndefined();
});

it('shows the notice anyway when the font fails to load', async () => {
	withFonts(() => Promise.reject(new Error('blocked')));
	await expect(fontsSettled()).resolves.toBeUndefined();
});

it('loads the open notice without counting it as proof of a session', async () => {
	resetNoticeCache();
	const ok = vi.fn();
	const off = onSessionOk(ok);
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => new Response(JSON.stringify({ version: 3, text: '# N' }), { status: 200 }))
	);
	await expect(loadNotice()).resolves.toEqual({ version: 3, text: '# N' });
	expect(ok).not.toHaveBeenCalled();
	off();
	vi.unstubAllGlobals();
});
