// "Needs X · Set up" in Sources (CMP-9b): the chip reads the structured state, Set up opens
// Settings → Companions' install dialog in place, nothing installs before the confirmation, the
// request echoes only the offer, a refusal shows its words, and the row reads the source again.
// Every wait is on a condition, never a delay.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import SourcesScreen from '../sources/SourcesScreen.svelte';
import { dht, source, sourceList } from '../sources/fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

const SHA = 'c'.repeat(64);
const OFFER = {
	id: 'qbittorrent',
	name: 'qBittorrent',
	version: '5.0.4',
	licence: 'GPL-2.0-or-later',
	size_bytes: 41_943_040,
	sha256: SHA,
	replaces: null
};
const waiting = (state: 'missing' | 'unhealthy' = 'missing', companion = 'qbittorrent') =>
	source({
		id: 5,
		name: 'Download client',
		kinds: ['importer'],
		companion_need: { companion, state }
	});

let net: ReturnType<typeof mockFetch>;
function setup(list = sourceList(dht, waiting())) {
	net = mockFetch();
	net.on('GET /sources', { body: list });
	net.on('GET /companions/qbittorrent/install', { body: OFFER });
	net.on('POST /companions/qbittorrent/install', { body: { id: 'qbittorrent' } });
	return render(SourcesScreen);
}
const installs = () => net.sent.filter((s) => s.method === 'POST');
const setUp = () => screen.findByTestId('companion-need-setup');
afterEach(() => vi.unstubAllGlobals());

describe('the companion chip in Sources', () => {
	it('reads the structured state: Needs X with Set up, nothing for a source that needs none', async () => {
		setup();
		await screen.findByTestId('source-5');
		const row = within(screen.getByTestId('source-5'));
		expect((await row.findByTestId('companion-need-text')).textContent).toBe('Needs qBittorrent');
		const button = row.getByTestId('companion-need-setup');
		expect(button.tagName).toBe('BUTTON');
		expect(button.textContent).toContain('Set up');
		expect(button.textContent).toContain('qBittorrent'); // for screen readers (sr-only)
		expect(within(screen.getByTestId('source-0')).queryByTestId('companion-need')).toBeNull();
		expect(installs()).toEqual([]);
	});

	it('names an unhealthy companion and offers no Set up', async () => {
		setup(sourceList(waiting('unhealthy', 'prowlarr')));
		await screen.findByTestId('source-5');
		expect((await screen.findByTestId('companion-need-text')).textContent).toBe(
			"Prowlarr isn't responding"
		);
		expect(screen.queryByTestId('companion-need-setup')).toBeNull();
	});

	it("opens the install dialog in place and shows the server's whole offer", async () => {
		setup();
		await fireEvent.click(await setUp());
		const dialog = await screen.findByRole('dialog');
		for (const text of ['qBittorrent', '5.0.4', 'GPL-2.0-or-later', SHA])
			expect(dialog.textContent).toContain(text);
		// Still on Sources, and nothing was sent but the offer's read.
		expect(screen.getByTestId('sources-screen')).toBeTruthy();
		expect(installs()).toEqual([]);
	});

	it("installs once on confirm, sending only the offer's version and SHA-256, then reads the source again", async () => {
		let reads = 0;
		net = mockFetch();
		net.on('GET /sources', () => ({
			body: sourceList(
				dht,
				++reads === 1 ? waiting() : source({ ...waiting(), companion_need: null })
			)
		}));
		net.on('GET /companions/qbittorrent/install', { body: OFFER });
		net.on('POST /companions/qbittorrent/install', { body: { id: 'qbittorrent' } });
		render(SourcesScreen);
		await fireEvent.click(await setUp());
		const dialog = await screen.findByRole('dialog');
		const confirm = within(dialog).getByRole('button', { name: 'Install' });
		// A double press before the dialog re-renders is still one request.
		// Raw events, back to back with no render between them (fireEvent flushes after the first).
		confirm.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		confirm.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await waitFor(() => expect(screen.queryByTestId('companion-need')).toBeNull());
		expect(installs()).toEqual([
			{
				method: 'POST',
				path: '/companions/qbittorrent/install',
				url: '/api/v1/companions/qbittorrent/install',
				body: JSON.stringify({ version: '5.0.4', sha256: SHA })
			}
		]);
		expect(reads).toBe(2);
		// The chip is gone, so focus goes to the row's name, not to <body>.
		await waitFor(() =>
			expect(document.activeElement).toBe(
				within(screen.getByTestId('source-5')).getByRole('heading')
			)
		);
	});

	it('shows a refused install in plain words and keeps the chip', async () => {
		setup();
		net.error('POST /companions/qbittorrent/install', 409, 'not_wired');
		await fireEvent.click(await setUp());
		const dialog = await screen.findByRole('dialog');
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Install' }));
		const words = await within(dialog).findByRole('alert');
		expect(words.textContent).not.toContain('not_wired');
		expect(words.textContent).not.toContain('server:');
		expect(screen.getByTestId('companion-need-text').textContent).toBe('Needs qBittorrent');
	});

	it('shows a refused offer in plain words and opens no dialog', async () => {
		setup();
		net.error('GET /companions/qbittorrent/install', 409, 'native_in_container');
		await fireEvent.click(await setUp());
		const alert = await screen.findByRole('alert');
		expect(alert.textContent).not.toContain('native_in_container');
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(installs()).toEqual([]);
	});

	it('tries the offer again when Set up is pressed after a refusal', async () => {
		setup();
		net.on(
			'GET /companions/qbittorrent/install',
			{ status: 409, body: { error: { code: 'busy', message: 'x' } } },
			{ body: OFFER }
		);
		await fireEvent.click(await setUp());
		await screen.findByRole('alert');
		await fireEvent.click(screen.getByTestId('companion-need-setup'));
		await screen.findByRole('dialog');
		expect(installs()).toEqual([]);
	});

	it('is reachable and operable from the keyboard, and the dialog opens on Cancel', async () => {
		setup();
		const button = await setUp();
		expect(button.tabIndex).toBeGreaterThanOrEqual(0);
		button.focus();
		expect(document.activeElement).toBe(button);
		await fireEvent.click(button); // Enter and Space on a button are a click
		const dialog = await screen.findByRole('dialog');
		await waitFor(() =>
			expect(document.activeElement).toBe(within(dialog).getByRole('button', { name: 'Cancel' }))
		);
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(installs()).toEqual([]);
	});
});
