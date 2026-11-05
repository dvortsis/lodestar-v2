// Part 2: the download client and rule-pack steps. Every blocking rule of the brief has a test, and
// each waits on a condition (a busy button settling, a request count), never on a delay.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { clearIndexCache } from '../settings/form/schema';
import { writeProgress, type StepId } from './steps';
import { PACK_IDS, PASSWORD, wizardServer } from './testing/server';
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

describe('rule-pack step, fresh install (RP-DEF): every pack on, opt out', () => {
	it('shows all five packs checked, as the server has them on', async () => {
		start('packs');
		await heading('Rule packs');
		for (const id of PACK_IDS) {
			const b = await box(id);
			expect(b.checked).toBe(true);
			expect(b.disabled).toBe(false);
		}
		expect(screen.queryByTestId('wizard-packs-offered')).toBeNull();
		expect(screen.getByText(/All five are on by default/)).toBeTruthy();
	});

	it('Continue with nothing changed sends nothing and marks the step done', async () => {
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Blocklists');
		expect(posts()).toEqual([]);
		expect(server.packs.every((p) => p.on)).toBe(true);
		expect(progress().done).toContain('packs');
	});

	it('Skip sends nothing, so packs stay on', async () => {
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Skip this step' }));
		await heading('Blocklists');
		expect(posts()).toEqual([]);
		expect(server.packs.every((p) => p.on)).toBe(true);
		expect(progress().skipped).toEqual(['packs']);
	});

	it('unchecking a pack goes through the weakening flow: the effect warning, then the password', async () => {
		start('packs');
		await fireEvent.click(await box('spam'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await screen.findByRole('dialog', { name: 'Turn off Spam' });
		expect((await screen.findByTestId('effect-warning-text')).textContent).toContain(
			'shows what it hid'
		);
		// Nothing is applied without the password.
		expect(applies()).toEqual([]);
		await fireEvent.input(await screen.findByTestId('change-password'), {
			target: { value: PASSWORD }
		});
		await fireEvent.click(await screen.findByTestId('change-apply'));
		await waitFor(() => expect(server.packs.find((p) => p.id === 'spam')!.on).toBe(false));
		await fireEvent.click(await screen.findByTestId('change-cancel'));
		await heading('Blocklists');
		expect(applies().map((c) => c.body)).toEqual([
			{ change: { op: 'pack_switch', id: 'spam', on: false }, password: PASSWORD }
		]);
		expect(server.packs.filter((p) => p.on).map((p) => p.id)).toEqual(
			PACK_IDS.filter((id) => id !== 'spam')
		);
		expect(progress().done).toContain('packs');
	});

	it('a cancelled weakening flow keeps the pack on, keeps the unchecks, and Continue retries', async () => {
		start('packs');
		await fireEvent.click(await box('fakes'));
		await fireEvent.click(await box('spam'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await screen.findByRole('dialog', { name: 'Turn off Fakes' });
		await fireEvent.click(await screen.findByTestId('change-cancel'));
		expect((await screen.findByTestId('wizard-packs-error')).textContent).toContain(
			"wasn't turned off"
		);
		await settled('Save and continue');
		expect(applies()).toEqual([]);
		expect(server.packs.every((p) => p.on)).toBe(true);
		// The user's choices are kept: both still unchecked, though both are still on.
		expect((await box('fakes')).checked).toBe(false);
		expect((await box('spam')).checked).toBe(false);
		expect(progress().step).toBe('packs');
		expect(progress().done).not.toContain('packs');
		// Continue tries again; this time Fakes is kept on, so only Spam's dialog opens.
		await fireEvent.click(await box('fakes'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await screen.findByRole('dialog', { name: 'Turn off Spam' });
		await fireEvent.input(await screen.findByTestId('change-password'), {
			target: { value: PASSWORD }
		});
		await fireEvent.click(await screen.findByTestId('change-apply'));
		await waitFor(() => expect(server.packs.find((p) => p.id === 'spam')!.on).toBe(false));
		await fireEvent.click(await screen.findByTestId('change-cancel'));
		await heading('Blocklists');
		expect(server.packs.find((p) => p.id === 'fakes')!.on).toBe(true);
		expect(applies()).toHaveLength(1);
	});

	it('turns one pack on and two off in one Continue: on first, then one password prompt each', async () => {
		server.packs.find((p) => p.id === 'lures')!.on = false;
		start('packs');
		await fireEvent.click(await box('lures'));
		await fireEvent.click(await box('spam'));
		await fireEvent.click(await box('scams'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		for (const name of ['Spam', 'Scams']) {
			await screen.findByRole('dialog', { name: `Turn off ${name}` });
			await fireEvent.input(await screen.findByTestId('change-password'), {
				target: { value: PASSWORD }
			});
			await fireEvent.click(await screen.findByTestId('change-apply'));
			await waitFor(() =>
				expect(server.packs.find((p) => p.id === name.toLowerCase())!.on).toBe(false)
			);
			await fireEvent.click(await screen.findByTestId('change-cancel'));
		}
		await heading('Blocklists');
		expect(applies().map((c) => c.body)).toEqual([
			{ change: { op: 'pack_switch', id: 'lures', on: true } },
			{ change: { op: 'pack_switch', id: 'spam', on: false }, password: PASSWORD },
			{ change: { op: 'pack_switch', id: 'scams', on: false }, password: PASSWORD }
		]);
		expect(server.packs.filter((p) => p.on).map((p) => p.id)).toEqual([
			'malware',
			'lures',
			'fakes',
			'unauthorised-copy'
		]);
	});

	it('a wrong password changes nothing', async () => {
		start('packs');
		await fireEvent.click(await box('scams'));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await fireEvent.input(await screen.findByTestId('change-password'), {
			target: { value: 'wrong' }
		});
		await fireEvent.click(await screen.findByTestId('change-apply'));
		await screen.findAllByText('That password is wrong. Enter it again.');
		expect(applies()).toHaveLength(1);
		expect(server.packs.every((p) => p.on)).toBe(true);
		await fireEvent.click(await screen.findByTestId('change-cancel'));
		await screen.findByTestId('wizard-packs-error');
		expect(progress().done).not.toContain('packs');
	});
});

describe('rule-pack step, existing install with packs off: offered, not flipped', () => {
	it("shows the server's off state unchecked and offers the packs", async () => {
		server.existingInstall();
		start('packs');
		await heading('Rule packs');
		await screen.findByTestId('wizard-packs-offered');
		for (const id of PACK_IDS) expect((await box(id)).checked).toBe(false);
	});

	it('Continue with nothing checked sends nothing, so packs stay off', async () => {
		server.existingInstall();
		start('packs');
		await box('malware');
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Blocklists');
		expect(posts()).toEqual([]);
		expect(server.packs.some((p) => p.on)).toBe(false);
	});

	it('checking packs turns them on, each through a preview and a plain apply, no password', async () => {
		server.existingInstall();
		start('packs');
		for (const id of PACK_IDS.filter((id) => id !== 'spam')) await fireEvent.click(await box(id));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Blocklists');
		const bodies = applies().map((c) => c.body as { change: { on: boolean }; password?: string });
		expect(bodies).toHaveLength(5);
		expect(bodies.every((b) => b.change.on === true && !('password' in b))).toBe(true);
		expect(posts().filter((c) => c.path === '/filters/preview/jobs')).toHaveLength(5);
		expect(server.packs.find((p) => p.id === 'spam')!.on).toBe(false);
		expect(progress().done).toContain('packs');
	});

	it('a failed apply stays on the step, shows the server’s state and the error, and is not done', async () => {
		let n = 0;
		server.onApply((body) => {
			if (++n === 2)
				return server.json(500, { error: { code: 'internal', message: 'The store is busy.' } });
			server.packs.find((p) => p.id === body.change.id)!.on = true;
			return server.json(200, { applied: true, assessment: strengthening(), version: 3 });
		});
		server.existingInstall();
		start('packs');
		await box('malware');
		for (const id of PACK_IDS) await fireEvent.click(await box(id));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		expect((await screen.findByTestId('wizard-packs-error')).textContent).toContain(
			'store is busy'
		);
		expect(screen.getByRole('heading', { level: 2, name: 'Rule packs' })).toBeTruthy();
		expect(progress().step).toBe('packs');
		expect(progress().done).not.toContain('packs');
		// The first pack went through; the screen now shows what the server has.
		await waitFor(() =>
			expect(server.filterCalls.filter((c) => c.method === 'GET').length).toBeGreaterThan(1)
		);
		expect((await box('malware')).checked).toBe(true);
		expect(applies()).toHaveLength(2);
		expect(server.packs.map((p) => p.on)).toEqual([true, false, false, false, false, false]);
		// The user's choice for the others is kept, so Continue tries them again.
		for (const id of PACK_IDS.slice(1)) expect((await box(id)).checked).toBe(true);
	});

	it('leaves a change that needs a confirmation to Settings: nothing is applied', async () => {
		server.setAssessment(weakening());
		server.existingInstall();
		start('packs');
		await box('malware');
		for (const id of PACK_IDS) await fireEvent.click(await box(id));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		expect((await screen.findByTestId('wizard-packs-error')).textContent).toContain('Settings');
		expect(applies()).toEqual([]);
		expect(progress().done).not.toContain('packs');
		expect(server.packs.some((p) => p.on)).toBe(false);
	});

	it('does not apply a change whose preview carries a warning', async () => {
		server.setAssessment(strengthening({ warning: 'This turns off a check.' }));
		server.existingInstall();
		start('packs');
		await box('malware');
		for (const id of PACK_IDS) await fireEvent.click(await box(id));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await settled('Save and continue');
		await screen.findByTestId('wizard-packs-error');
		expect(applies()).toEqual([]);
		expect(server.packs.some((p) => p.on)).toBe(false);
		expect(progress().done).not.toContain('packs');
	});

	it('re-checks and applies again when the change moved since its preview', async () => {
		let n = 0;
		server.onApply((body) => {
			if (++n === 1)
				return server.json(409, {
					error: { code: 'changed_since_preview', message: 'The rules changed.' }
				});
			server.packs.find((p) => p.id === body.change.id)!.on = true;
			return server.json(200, { applied: true, assessment: strengthening(), version: 3 });
		});
		server.existingInstall();
		start('packs');
		await box('malware');
		for (const id of PACK_IDS) await fireEvent.click(await box(id));
		await fireEvent.click(await screen.findByRole('button', { name: 'Save and continue' }));
		await heading('Blocklists');
		const bodies = applies().map((c) => c.body as { change: { id: string; on: boolean } });
		expect(bodies).toHaveLength(7);
		expect(bodies.every((b) => b.change.on === true)).toBe(true);
		expect(server.packs.every((p) => p.on)).toBe(true);
	});

	it('does not apply a weakening change even when the server asks for no password', async () => {
		server.setAssessment(weakening({ confirm: false }));
		server.existingInstall();
		start('packs');
		await box('malware');
		for (const id of PACK_IDS) await fireEvent.click(await box(id));
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
		await fireEvent.input(await screen.findByLabelText('Address'), {
			target: { value: 'http://qbittorrent.local:8080' }
		});
		await fireEvent.click(await screen.findByRole('button', { name: /Test connection/ }));
		await waitFor(() =>
			expect(
				server.calls.filter((c) => c.method === 'POST' && c.path === '/settings/test')
			).toHaveLength(1)
		);
		// Testing saves nothing and doesn't finish the step.
		expect(server.requests('PATCH', '/settings')).toEqual([]);
		expect(progress().done).toEqual([]);
		await fireEvent.click(screen.getByRole('button', { name: 'Skip this step' }));
		await heading('Rule packs');
		expect(server.requests('PATCH', '/settings')).toEqual([]);
		expect(
			server.calls.filter((c) => c.method !== 'GET').map((c) => `${c.method} ${c.path}`)
		).toEqual(['POST /settings/test']);
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
