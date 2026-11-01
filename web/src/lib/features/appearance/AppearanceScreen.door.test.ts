// Show adult content loads its 18+ flow through the filters flows' door, on the first change only
// (X-flows-door-2). The door is the real one, wrapped in a spy.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';

const get = vi.hoisted(() => vi.fn());
const patch = vi.hoisted(() => vi.fn());
const loadAdult = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get, patch }
}));
vi.mock('../filters/flows-door', async (orig) => {
	const real = await orig<typeof import('../filters/flows-door')>();
	loadAdult.mockImplementation(real.loadAdult);
	return { ...real, loadAdult };
});

import AppearanceScreen from './AppearanceScreen.svelte';

configure({ defaultHidden: true });

const field = (value: unknown, extra = {}) => ({ value, env: null, read_only: false, ...extra });
let adultField = field(false);
const toggle = () =>
	screen.findByRole('switch', { name: 'Show adult content' }) as Promise<HTMLInputElement>;
const contentCalls = () =>
	patch.mock.calls.filter((c) => c[0] === '/settings/{section}' && c[1].path.section === 'content');

beforeEach(() => {
	vi.stubGlobal('matchMedia', () => ({
		matches: false,
		addEventListener() {},
		removeEventListener() {}
	}));
	adultField = field(false);
	get.mockReset().mockImplementation(async (_p: string, o: { path: { section: string } }) =>
		o.path.section === 'content'
			? { section: 'content', revision: 2, fields: { show_adult: adultField } }
			: {
					section: 'preferences',
					revision: 4,
					fields: Object.fromEntries(
						Object.entries({
							theme: 'riverstone',
							theme_dark: '',
							mode: 'system',
							density: 'comfortable',
							motion: 'system',
							illustrations: true
						}).map(([k, v]) => [k, field(v)])
					)
				}
	);
	patch.mockReset().mockResolvedValue({ revision: 5 });
	loadAdult.mockClear();
});

describe("Show adult content through the flows' door", () => {
	it('loads nothing of the flows until the first change, then the 18+ dialog through the door', async () => {
		render(AppearanceScreen);
		await toggle();
		expect(loadAdult).not.toHaveBeenCalled();
		await fireEvent.click(await toggle());
		await screen.findByRole('dialog', { name: 'Show adult content?' }, { timeout: 10_000 });
		expect(loadAdult).toHaveBeenCalledTimes(1);
		expect(contentCalls()).toHaveLength(0);
		await fireEvent.click(screen.getByTestId('adult-show'));
		await waitFor(() => expect(contentCalls()).toHaveLength(1), { timeout: 10_000 });
		expect(contentCalls()[0]![1].body).toEqual({ show_adult: true, confirm_adult: true });
		await waitFor(() =>
			expect(
				(screen.getByRole('switch', { name: 'Show adult content' }) as HTMLInputElement).checked
			).toBe(true)
		);
	});

	it('a door that fails to load leaves the switch off, says so and sends nothing', async () => {
		loadAdult.mockRejectedValueOnce(new Error('offline'));
		render(AppearanceScreen);
		await fireEvent.click(await toggle());
		expect(await screen.findByText(/Couldn't change this setting/)).toBeTruthy();
		await waitFor(() => expect(loadAdult).toHaveBeenCalledTimes(1));
		expect((await toggle()).checked).toBe(false);
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(contentCalls()).toHaveLength(0);
	});

	it('turning it off loads nothing of the flows and asks nothing', async () => {
		adultField = field(true);
		render(AppearanceScreen);
		const on = await toggle();
		expect(on.checked).toBe(true);
		await fireEvent.click(on);
		await waitFor(() => expect(contentCalls()).toHaveLength(1), { timeout: 10_000 });
		expect(contentCalls()[0]![1].body).toEqual({ show_adult: false });
		expect(loadAdult).not.toHaveBeenCalled();
		expect(screen.queryByRole('dialog')).toBeNull();
	});

	it('names the variable when the server says the switch is read-only', async () => {
		adultField = field(false, { read_only: true });
		render(AppearanceScreen);
		expect((await toggle()).disabled).toBe(true);
		expect(screen.getByText(/LODESTAR_SHOW_ADULT/)).toBeTruthy();
	});
});
