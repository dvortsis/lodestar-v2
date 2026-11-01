// Part 2: the download client and rule-pack steps. Every blocking rule of the brief has a test, and
// each waits on a condition (a busy button settling, a request count), never on a delay.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { clearIndexCache } from '../settings/form/schema';
import { writeProgress, type StepId } from './steps';
import { PACK_IDS, wizardServer } from './testing/server';
import { strengthening, weakening } from '../filters/flows/testing/fixtures';

vi.mock('$app/paths', () => ({
	resolve: (path: string, params?: Record<string, string>) =>
		params ? path.replace(/\[(\w+)\]/g, (_, k: string) => params[k]!) : path
}));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));
import WizardPage from './WizardPage.svelte';

vi.setConfig({ testTimeout: 40_000 });
configure({ asyncUtilTimeout: 20_000 });

let server: ReturnType<typeof wizardServer>;
const start = (step: StepId) => {
	vi.stubGlobal('fetch', server.fetch);
	writeProgress({ step, skipped: [], done: [] });
	return render(WizardPage);
};
const heading = (name: string) => screen.findByRole('heading', { level: 2, name });
const progress = () => JSON.parse(sessionStorage.getItem('lodestar.wizard')!);
const posts = () => server.filterCalls.filter((c) => c.method === 'POST');
const applies = () => posts().filter((c) => c.path === '/filters/apply');
const settled = async (name: string) => {
	const button = await screen.findByRole('button', { name });
	await waitFor(() => expect(button.getAttribute('aria-busy')).toBeNull());
	return button;
};
const box = (id: string) => screen.findByTestId(`wizard-pack-${id}`) as Promise<HTMLInputElement>;

beforeEach(() => {
	sessionStorage.clear();
	clearIndexCache();
	server = wizardServer();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('rule-pack step', () => {
	it('pre-checks all five packs when the server has none on', async () => {
		start('packs');
		await heading('Rule packs');
		for (const id of PACK_IDS) expect((await box(id)).checked).toBe(true);
		expect(screen.queryByTestId('wizard-packs-stored')).toBeNull();
	});

	it('Continue turns the checked packs on, each through a preview and a plain apply', async () => {
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Updates');
		expect(applies().map((c) => c.body)).toEqual(
			PACK_IDS.map((id) => ({ change: { op: 'pack_switch', id, on: true } }))
		);
		expect(posts().filter((c) => c.path === '/filters/preview/jobs')).toHaveLength(5);
		expect(server.packs.every((p) => p.on)).toBe(true);
		expect(progress().done).toContain('packs');
	});

	it('only ever turns packs on and never sends a password', async () => {
		start('packs');
		await fireEvent.click(await box('spam'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Updates');
		const bodies = applies().map((c) => c.body as { change: { on: boolean }; password?: string });
		expect(bodies).toHaveLength(4);
		expect(bodies.every((b) => b.change.on === true && !('password' in b))).toBe(true);
		expect(server.packs.find((p) => p.id === 'spam')!.on).toBe(false);
	});

	it('Skip sends nothing, so packs stay off', async () => {
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Skip this step' }));
		await heading('Updates');
		expect(posts()).toEqual([]);
		expect(server.packs.some((p) => p.on)).toBe(false);
		expect(progress().skipped).toEqual(['packs']);
	});

	it('Continue with none checked sends nothing', async () => {
		start('packs');
		for (const id of PACK_IDS) await fireEvent.click(await box(id));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Updates');
		expect(posts()).toEqual([]);
		expect(server.packs.some((p) => p.on)).toBe(false);
	});

	it('shows a stored choice as the server has it, not pre-checked, and cannot turn a pack off', async () => {
		server.packs[0]!.on = true;
		start('packs');
		await heading('Rule packs');
		await screen.findByTestId('wizard-packs-stored');
		const malware = await box('malware');
		expect(malware.checked).toBe(true);
		expect(malware.disabled).toBe(true);
		for (const id of PACK_IDS.slice(1)) expect((await box(id)).checked).toBe(false);
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Updates');
		expect(posts()).toEqual([]);
	});

	it('a failed apply stays on the step, shows the server’s state and the error, and is not done', async () => {
		let n = 0;
		server.onApply((body) => {
			if (++n === 2)
				return server.json(500, { error: { code: 'internal', message: 'The store is busy.' } });
			server.packs.find((p) => p.id === body.change.id)!.on = true;
			return server.json(200, { applied: true, assessment: strengthening(), version: 3 });
		});
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		expect((await screen.findByTestId('wizard-packs-error')).textContent).toContain(
			'store is busy'
		);
		expect(screen.getByRole('heading', { level: 2, name: 'Rule packs' })).toBeTruthy();
		expect(progress().step).toBe('packs');
		expect(progress().done).not.toContain('packs');
		// The first pack went through; the screen now shows what the server has.
		await waitFor(async () => expect((await box('malware')).disabled).toBe(true));
		expect(applies()).toHaveLength(2);
		expect(server.packs.map((p) => p.on)).toEqual([true, false, false, false, false]);
		// The user's choice for the others is kept, so Continue tries them again.
		for (const id of PACK_IDS.slice(1)) expect((await box(id)).checked).toBe(true);
	});

	it('leaves a change that needs a confirmation to Settings: nothing is applied', async () => {
		server.setAssessment(weakening());
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		expect((await screen.findByTestId('wizard-packs-error')).textContent).toContain('Settings');
		expect(applies()).toEqual([]);
		expect(progress().done).not.toContain('packs');
		expect(server.packs.some((p) => p.on)).toBe(false);
	});

	it('does not apply a weakening change even when the server asks for no password', async () => {
		server.setAssessment(weakening({ confirm: false }));
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		await screen.findByTestId('wizard-packs-error');
		expect(applies()).toEqual([]);
		expect(server.packs.some((p) => p.on)).toBe(false);
	});

	it('does not move on, or mark done, when the packs could not be read', async () => {
		server.failFilters(true);
		start('packs');
		await heading('Rule packs');
		await screen.findByRole('button', { name: 'Retry' });
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		expect(screen.getByRole('heading', { level: 2, name: 'Rule packs' })).toBeTruthy();
		expect(progress().done).toEqual([]);
		expect(applies()).toEqual([]);
	});
});

describe('download client step', () => {
	it('shows the client settings with the recommended client named', async () => {
		start('download');
		await heading('Download client');
		expect((await screen.findByText(/qBittorrent is the recommended/)).textContent).toBeTruthy();
		await screen.findByRole('button', { name: 'Add a download client' });
	});

	it('offers Test connection on an added client, and Skip sends nothing', async () => {
		start('download');
		await fireEvent.click(await screen.findByRole('button', { name: 'Add a download client' }));
		await screen.findByRole('button', { name: /Test connection/ });
		await fireEvent.click(screen.getByRole('button', { name: 'Skip this step' }));
		await heading('Rule packs');
		expect(server.requests('PATCH', '/settings')).toEqual([]);
		expect(server.calls.filter((c) => c.method === 'POST')).toEqual([]);
		expect(progress().skipped).toEqual(['download']);
	});

	it('does not move on, or mark done, when the settings schema could not be read', async () => {
		server.failIndex(true);
		start('download');
		await heading('Download client');
		await screen.findByText(/Couldn't load the download client settings/);
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		expect(screen.getByRole('heading', { level: 2, name: 'Download client' })).toBeTruthy();
		expect(progress().done).toEqual([]);
	});

	it('does not move on, or mark done, when the client settings could not be read', async () => {
		server.next('GET /settings/integrations', server.err(500, 'internal'));
		start('download');
		await heading('Download client');
		await screen.findByRole('button', { name: 'Retry' });
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		expect(screen.getByRole('heading', { level: 2, name: 'Download client' })).toBeTruthy();
		expect(progress().done).toEqual([]);
	});
});
