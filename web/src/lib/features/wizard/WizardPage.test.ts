import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { writeProgress, type StepId } from './steps';
import { lockedUpdates, wizardValues } from './testing/fixtures';
import { network, wizardServer } from './testing/server';

vi.mock('$app/paths', () => ({
	resolve: (path: string, params?: Record<string, string>) =>
		params ? path.replace(/\[(\w+)\]/g, (_, k: string) => params[k]!) : path
}));
vi.mock('$app/navigation', () => ({ beforeNavigate: () => {}, goto: vi.fn() }));
import WizardPage from './WizardPage.svelte';

vi.setConfig({ testTimeout: 30_000 });
configure({ asyncUtilTimeout: 15_000 });

let server: ReturnType<typeof wizardServer>;
const start = (step: StepId, over: Parameters<typeof wizardValues>[0] = {}) => {
	server = wizardServer(wizardValues(over));
	vi.stubGlobal('fetch', server.fetch);
	writeProgress({ step, skipped: [], done: [] });
	return render(WizardPage);
};
const patches = () => server.requests('PATCH', '/settings');
const heading = (name: string) => screen.findByRole('heading', { level: 2, name });

beforeEach(() => {
	sessionStorage.clear();
	vi.spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('wizard shell', () => {
	it('shows steps 1 and 2 as done and the rest as steps, one marked current', async () => {
		start('network');
		await heading('Network check');
		const list = screen.getByTestId('wizard-progress');
		expect(list).toBeTruthy();
		expect(screen.getByText('Setup token')).toBeTruthy();
		expect(screen.getByTestId('wizard-step-network').getAttribute('aria-current')).toBe('step');
		expect(screen.getByTestId('wizard-step-how').getAttribute('data-state')).toBe('later');
		// Status is icon plus text, never colour alone.
		expect(
			within(screen.getByTestId('wizard-step-network')).getByText(/Current step/)
		).toBeTruthy();
	});

	it('resumes after a reload and moves focus to each step heading', async () => {
		start('how');
		await heading('How it works');
		expect(screen.queryByRole('button', { name: 'Back' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
		const next = await heading('Crawl intensity');
		await waitFor(() => expect(document.activeElement).toBe(next));
		expect(JSON.parse(sessionStorage.getItem('lodestar.wizard')!)).toMatchObject({
			step: 'intensity',
			done: ['how']
		});
	});

	it('has no Back on the first step and no Skip on the informational ones', async () => {
		start('network');
		await heading('Network check');
		expect(screen.queryByRole('button', { name: 'Back' })).toBeNull();
		expect(screen.getByRole('button', { name: 'Skip this step' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
		await heading('How it works');
		expect(screen.queryByRole('button', { name: 'Skip this step' })).toBeNull();
		await fireEvent.click(screen.getByRole('button', { name: 'Back' }));
		await heading('Network check');
	});

	it('Skip sends nothing, even with an edit in the field', async () => {
		start('intensity');
		await heading('Crawl intensity');
		await fireEvent.click(await screen.findByRole('radio', { name: 'low' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Skip this step' }));
		await heading('Storage & retention');
		expect(patches()).toHaveLength(0);
		expect(server.calls.filter((c) => c.method !== 'GET')).toEqual([]);
		expect(JSON.parse(sessionStorage.getItem('lodestar.wizard')!).skipped).toEqual(['intensity']);
	});

	it('Save and continue sends one patch and moves on', async () => {
		start('intensity');
		await heading('Crawl intensity');
		await fireEvent.click(await screen.findByRole('radio', { name: 'low' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Save and continue' }));
		await heading('Storage & retention');
		expect(patches().map((c) => c.body)).toEqual([
			{ crawler: { if_revision: 3, intensity: 'low' } }
		]);
	});

	it('continues without a request when nothing was changed', async () => {
		start('intensity');
		await heading('Crawl intensity');
		await screen.findByRole('radio', { name: 'low' });
		await fireEvent.click(screen.getByRole('button', { name: 'Save and continue' }));
		await heading('Storage & retention');
		expect(patches()).toHaveLength(0);
	});

	it('a refused save stays on the step with the engine’s error and the server’s value', async () => {
		start('intensity');
		await heading('Crawl intensity');
		await fireEvent.click(await screen.findByRole('radio', { name: 'low' }));
		server.next(
			'PATCH /settings',
			server.err(422, 'invalid_setting', { section: 'crawler', field: 'intensity' })
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Save and continue' }));
		await screen.findByTestId('error-summary');
		expect(screen.getByRole('heading', { level: 2, name: 'Crawl intensity' })).toBeTruthy();
		expect(screen.queryByRole('heading', { level: 2, name: 'Storage & retention' })).toBeNull();
		expect(JSON.parse(sessionStorage.getItem('lodestar.wizard')!).step).toBe('intensity');
		// The server still holds its own value.
		expect(server.sections.get('crawler')!.fields.intensity!.value).toBe('balanced');
	});

	it('an unreachable server also stays on the step', async () => {
		start('storage');
		await heading('Storage & retention');
		await fireEvent.click(await screen.findByRole('switch'));
		server.next('PATCH /settings', { status: 0, body: null });
		await fireEvent.click(screen.getByRole('button', { name: 'Save and continue' }));
		await waitFor(() => expect(patches()).toHaveLength(1));
		expect(screen.getByRole('heading', { level: 2, name: 'Storage & retention' })).toBeTruthy();
	});
});

describe('network check step', () => {
	it('shows a reachable port with its address', async () => {
		start('network');
		await screen.findAllByText('Reachable');
		expect(screen.getByTestId('wizard-net-address').textContent).toContain('203.0.113.9:6881');
		expect(screen.queryByText('If it stays unreachable')).toBeNull();
	});

	it('explains an unreachable port: forwarding and VPN', async () => {
		start('network');
		server.setNetwork(network({ reachable: false, reason: 'no_unsolicited_queries' }));
		await fireEvent.click(await screen.findByRole('button', { name: 'Retest' }));
		await screen.findAllByText('Not reachable yet');
		expect(screen.getByText('If it stays unreachable')).toBeTruthy();
		expect(screen.getByText(/forward the DHT port/)).toBeTruthy();
		expect(screen.getByText(/Behind a VPN/)).toBeTruthy();
	});

	it('says plainly when Retest is rate limited', async () => {
		start('network');
		await screen.findAllByText('Reachable');
		server.setNetwork(() =>
			server.json(429, { error: { code: 'rate_limited', message: 'x' } }, { 'Retry-After': '20' })
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Retest' }));
		await waitFor(() =>
			expect(screen.getByTestId('wizard-net-limited').textContent).toContain(
				'Try again in 20 seconds'
			)
		);
	});

	it('aborts the read in flight when the step is left', async () => {
		server = wizardServer();
		server.setNetwork(network({ reachable: null, reason: 'measuring' }));
		const inner = server.fetch;
		const signals: AbortSignal[] = [];
		vi.stubGlobal('fetch', (url: string, init: RequestInit = {}) => {
			if (url.endsWith('/network/check') && init.signal) signals.push(init.signal);
			return inner(url, init);
		});
		writeProgress({ step: 'network', skipped: [], done: [] });
		render(WizardPage);
		await screen.findByText('Measuring');
		await fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
		await heading('How it works');
		expect(signals.length).toBeGreaterThan(0);
		await waitFor(() => expect(signals.every((s) => s.aborted)).toBe(true));
	});

	it('treats a turned-off DHT as final: no progress, no port-forward help', async () => {
		server = wizardServer();
		server.setNetwork(network({ reachable: null, reason: 'dht_off', measured_s: 0 }));
		vi.stubGlobal('fetch', server.fetch);
		writeProgress({ step: 'network', skipped: [], done: [] });
		render(WizardPage);
		await screen.findByText('The DHT is off');
		expect(screen.queryByTestId('wizard-net-progress')).toBeNull();
		expect(screen.queryByText('If it stays unreachable')).toBeNull();
	});
});

describe('how it works', () => {
	it('lists the four stages as text and sets the cold-start expectation', async () => {
		start('how');
		await heading('How it works');
		const items = within(await screen.findByTestId('wizard-how')).getAllByRole('listitem');
		expect(items.map((i) => i.textContent)).toEqual([
			expect.stringContaining('Listen to the DHT'),
			expect.stringContaining('Fetch metadata'),
			expect.stringContaining('Index'),
			expect.stringContaining('Search')
		]);
		expect(screen.getByTestId('wizard-how-cold').textContent).toMatch(/days and weeks/);
		expect(document.querySelector('svg')?.getAttribute('aria-hidden')).toBe('true');
	});
});

describe('updates step', () => {
	it('preselects only the server’s value (Off) and says checking contacts GitHub', async () => {
		start('updates');
		await heading('Updates');
		const off = await screen.findByRole('radio', { name: 'Off' });
		await waitFor(() => expect((off as HTMLInputElement).checked).toBe(true));
		expect(
			(screen.getByRole('radio', { name: 'Check and notify' }) as HTMLInputElement).checked
		).toBe(false);
		expect(
			(screen.getByRole('radio', { name: 'Install automatically' }) as HTMLInputElement).checked
		).toBe(false);
		expect(screen.getByTestId('wizard-updates-github').textContent).toContain('GitHub');
		expect(screen.queryByRole('radio', { name: 'Container image updates' })).toBeNull();
	});

	it('saves the choice through the multi-section patch', async () => {
		start('updates');
		await fireEvent.click(await screen.findByRole('radio', { name: 'Check and notify' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Save and continue' }));
		await heading('Done');
		expect(patches().map((c) => c.body)).toEqual([{ updates: { if_revision: 1, mode: 'notify' } }]);
	});

	it('shows a value set by the environment as locked', async () => {
		start('updates', { updates: lockedUpdates() });
		const notify = await screen.findByRole('radio', { name: 'Check and notify' });
		await waitFor(() => expect((notify as HTMLInputElement).checked).toBe(true));
		expect(notify.matches(':disabled')).toBe(true);
		expect(screen.getByTestId('wizard-updates-locked').textContent).toContain('LODESTAR_UPDATES');
	});
});

describe('restart and load failures', () => {
	it('says on Done that a saved storage field needs a restart', async () => {
		start('storage');
		await fireEvent.click(await screen.findByRole('switch'));
		server.next('PATCH /settings', (body) => {
			const sv = server.sections.get('storage')!;
			sv.revision++;
			sv.fields.archive_raw!.value = (
				body as { storage: { archive_raw: boolean } }
			).storage.archive_raw;
			return {
				status: 200,
				body: {
					sections: {
						storage: { ...structuredClone(sv), restart_required: ['archive_raw'] }
					}
				}
			};
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Save and continue' }));
		await heading('Updates');
		await fireEvent.click(screen.getByRole('button', { name: 'Skip this step' }));
		expect((await screen.findByTestId('wizard-restart')).textContent).toContain('Restart');
	});

	it('does not move on, or mark done, when the settings could not be read', async () => {
		server = wizardServer();
		server.next('GET /settings/crawler', server.err(500, 'internal'));
		vi.stubGlobal('fetch', server.fetch);
		writeProgress({ step: 'intensity', skipped: [], done: [] });
		render(WizardPage);
		await heading('Crawl intensity');
		await screen.findByRole('button', { name: 'Retry' });
		await fireEvent.click(screen.getByRole('button', { name: 'Save and continue' }));
		expect(screen.getByRole('heading', { level: 2, name: 'Crawl intensity' })).toBeTruthy();
		expect(JSON.parse(sessionStorage.getItem('lodestar.wizard')!).done).toEqual([]);
	});
});

describe('done', () => {
	it('links to Search and Settings, and to the screens of skipped steps', async () => {
		server = wizardServer();
		vi.stubGlobal('fetch', server.fetch);
		writeProgress({ step: 'done', skipped: ['intensity', 'updates'], done: ['network'] });
		render(WizardPage);
		await heading('Done');
		expect((await screen.findByRole('link', { name: 'Go to Search' })).getAttribute('href')).toBe(
			'/search'
		);
		expect(screen.getByRole('link', { name: 'Review settings' }).getAttribute('href')).toBe(
			'/settings'
		);
		const skipped = within(screen.getByTestId('wizard-skipped'));
		expect(skipped.getByRole('link', { name: 'Crawl intensity' }).getAttribute('href')).toBe(
			'/settings/crawler'
		);
		expect(skipped.getByRole('link', { name: 'Updates' }).getAttribute('href')).toBe(
			'/settings/updates'
		);
		expect(skipped.queryByRole('link', { name: 'Network check' })).toBeNull();
		expect(screen.queryByRole('button', { name: 'Continue' })).toBeNull();
	});
});
