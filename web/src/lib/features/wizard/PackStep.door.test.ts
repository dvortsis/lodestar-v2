// The pack step loads the change flow through the filters flows' door, on Continue only
// (X-flows-door-2). The door is the real one, wrapped in a spy.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen } from '@testing-library/svelte';
import { clearIndexCache } from '../settings/form/schema';
import { writeProgress } from './steps';
import { PACK_IDS, wizardServer } from './testing/server';

const load = vi.hoisted(() => vi.fn());
vi.mock('../filters/flows-door', async (orig) => {
	const real = await orig<typeof import('../filters/flows-door')>();
	load.mockImplementation(real.load);
	return { ...real, load };
});
vi.mock('$app/paths', () => ({
	resolve: (path: string, params?: Record<string, string>) =>
		params ? path.replace(/\[(\w+)\]/g, (_, k: string) => params[k]!) : path
}));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));
import WizardPage from './WizardPage.svelte';

vi.setConfig({ testTimeout: 40_000 });
configure({ asyncUtilTimeout: 20_000 });

let server: ReturnType<typeof wizardServer>;

beforeEach(() => {
	sessionStorage.clear();
	clearIndexCache();
	server = wizardServer();
	load.mockClear();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	vi.stubGlobal('fetch', server.fetch);
	writeProgress({ step: 'packs', skipped: [], done: [] });
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe("rule-pack step through the flows' door", () => {
	it('loads the change flow through the door on Continue, not before', async () => {
		render(WizardPage);
		await screen.findByTestId('wizard-pack-malware');
		expect(load).not.toHaveBeenCalled();
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await screen.findByRole('heading', { level: 2, name: 'Updates' });
		expect(load).toHaveBeenCalledTimes(1);
		expect(server.packs.map((p) => p.id)).toEqual(PACK_IDS);
		expect(server.packs.every((p) => p.on)).toBe(true);
	});
});
