import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { addRule, coreSwitch } from './changes';
import ChangeDialog from './ChangeDialog.svelte';
import AdultConfirmDialog from './AdultConfirmDialog.svelte';
import ImpactSummary from './ImpactSummary.svelte';
import EffectWarning from './EffectWarning.svelte';
import { createAdultConfirm, setShowAdult } from './adult.svelte';
import { createChangeFlow } from './flow.svelte';
import { mockFetch } from './testing/fetch';
import { HOSTILE, applied, finished, preview, strengthening, weakening } from './testing/fixtures';

configure({ defaultHidden: true });

const PW = 'tr0ub4dor&3-secret';
const off = coreSwitch('CS-4a', false);

let net: ReturnType<typeof mockFetch>;
beforeEach(() => {
	net = mockFetch();
	net.on('POST /filters/preview/jobs', { status: 202, body: { job: 7 } });
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
});
afterEach(() => {
	vi.unstubAllGlobals();
	localStorage.clear();
	sessionStorage.clear();
});

const noMarkup = (root: ParentNode) => {
	expect(root.querySelector('img, script')).toBeNull();
};

describe('ImpactSummary', () => {
	it('shows every field on its own row, never summed, ≈ only for estimates', () => {
		const { unmount } = render(ImpactSummary, { props: { preview: preview() } });
		const rows = {
			quarantine: '3',
			tombstones: '17',
			stored: '120',
			removed_from_floor: '5',
			caught_30d: '214',
			core_caught_30d: '9',
			per_day: '7/d'
		};
		for (const [k, v] of Object.entries(rows))
			expect(screen.getByTestId(`impact-${k}`).textContent?.trim()).toBe(v);
		expect(screen.getAllByRole('row')).toHaveLength(7);
		// No sum of rows appears (3 + 17, 3 + 5, 214 + 9, every row together, …).
		expect(document.body.textContent).not.toMatch(/\b(20|8|22|223|140|137|368|375)\b/);
		expect(document.body.textContent).not.toContain('≈');
		expect(screen.queryByTestId('impact-estimate')).toBeNull();
		unmount();

		render(ImpactSummary, { props: { preview: preview({ estimate: true }) } });
		expect(screen.getByTestId('impact-quarantine').textContent).toContain('≈');
		expect(screen.getByTestId('impact-per_day').textContent).toContain('≈');
		expect(screen.getByTestId('impact-estimate')).toBeTruthy();
	});

	it('puts both CSX-5 strengths side by side', () => {
		render(ImpactSummary, {
			props: { preview: preview(), neutralize: preview({ quarantine: 1, core_caught_30d: 0 }) }
		});
		expect(screen.getByTestId('impact-override-quarantine').textContent?.trim()).toBe('3');
		expect(screen.getByTestId('impact-neutralize-quarantine').textContent?.trim()).toBe('1');
		expect(screen.getByRole('columnheader', { name: 'Override' })).toBeTruthy();
		expect(screen.getByRole('columnheader', { name: 'Neutralize term' })).toBeTruthy();
	});
});

describe('EffectWarning', () => {
	it('renders the server text literally', () => {
		const { container } = render(EffectWarning, { props: { text: HOSTILE } });
		expect(screen.getByTestId('effect-warning-text').textContent).toBe(HOSTILE);
		noMarkup(container);
	});
});

async function openWith(assessment = weakening()) {
	net.on('GET /filters/preview/jobs/7', { body: finished(assessment) });
	const flow = createChangeFlow();
	const onapplied = vi.fn();
	render(ChangeDialog, { props: { flow, title: 'Turn off CS-4a', onapplied } });
	flow.start(off);
	await screen.findByRole('dialog', { name: 'Turn off CS-4a' });
	await waitFor(
		() => expect(screen.getByTestId('change-dialog').dataset.state).toMatch(/^(review|confirm)$/),
		{ timeout: 2000 }
	);
	return { flow, onapplied };
}

const password = () => document.querySelector<HTMLInputElement>('input[name="password"]');

describe('ChangeDialog', () => {
	it('a weakening change: warning, password field described by it, apply, What changed', async () => {
		net.on('POST /filters/apply', { body: applied(weakening(), { released: 3 }) });
		const { onapplied } = await openWith();
		const input = password()!;
		expect(input.type).toBe('password');
		expect(input.autocomplete).toBe('current-password');
		expect(document.querySelector('input[name="username"]')?.getAttribute('autocomplete')).toBe(
			'username'
		);
		const warningId = screen.getByTestId('effect-warning-text').parentElement!.id;
		expect(input.getAttribute('aria-describedby')).toContain(warningId);
		await waitFor(() => expect(document.activeElement).toBe(input));
		await fireEvent.input(input, { target: { value: PW } });
		await fireEvent.submit(input.form!);
		expect(input.value).toBe('');
		await screen.findByText('What changed');
		expect(screen.getByTestId('changed-released').textContent?.trim()).toBe('3');
		expect(onapplied).toHaveBeenCalledTimes(1);
		expect(password()).toBeNull();
	});

	it('a strengthening change shows no warning and no password field', async () => {
		await openWith(strengthening());
		expect(screen.queryByTestId('effect-warning')).toBeNull();
		expect(password()).toBeNull();
		expect(screen.getByTestId('change-apply')).toBeTruthy();
	});

	it('an empty password sends nothing', async () => {
		await openWith();
		await fireEvent.submit(password()!.form!);
		expect(net.to('/filters/apply')).toHaveLength(0);
		expect(screen.getByTestId('field-error').textContent).toContain('Enter your password');
	});

	it('a wrong password clears the field and shows the error', async () => {
		net.error('POST /filters/apply', 401, 'bad_password', { field: 'password' });
		await openWith();
		await fireEvent.input(password()!, { target: { value: PW } });
		await fireEvent.submit(password()!.form!);
		await waitFor(() => expect(screen.getByTestId('field-error').textContent).toContain('wrong'));
		expect(password()!.value).toBe('');
	});

	it("shows the server's error text literally", async () => {
		net.error('POST /filters/apply', 409, 'floor_locked');
		net.on('POST /filters/apply', {
			status: 409,
			body: { error: { code: 'floor_locked', message: HOSTILE } }
		});
		await openWith();
		await fireEvent.input(password()!, { target: { value: PW } });
		await fireEvent.submit(password()!.form!);
		const e = await screen.findByTestId('change-error');
		expect(e.textContent).toBe(HOSTILE);
		noMarkup(document.body);
	});

	it('the warning from the server is text, never markup', async () => {
		await openWith(weakening({ warning: HOSTILE }));
		expect(screen.getByTestId('effect-warning-text').textContent).toBe(HOSTILE);
		noMarkup(document.body);
	});

	it('the password reaches no storage, URL or console', async () => {
		const spies = (['log', 'info', 'warn', 'error', 'debug'] as const).map((k) =>
			vi.spyOn(console, k)
		);
		net.error('POST /filters/apply', 401, 'bad_password');
		await openWith();
		await fireEvent.input(password()!, { target: { value: PW } });
		await fireEvent.submit(password()!.form!);
		await screen.findByTestId('field-error');
		net.on('POST /filters/apply', { body: applied(weakening()) });
		await fireEvent.input(password()!, { target: { value: PW } });
		await fireEvent.submit(password()!.form!);
		await screen.findByText('What changed');
		const stored = JSON.stringify({ ...localStorage }) + JSON.stringify({ ...sessionStorage });
		expect(stored).not.toContain(PW);
		expect(location.href).not.toContain(PW);
		for (const s of spies) expect(JSON.stringify(s.mock.calls)).not.toContain(PW);
		expect(document.body.innerHTML).not.toContain(PW);
		for (const s of spies) s.mockRestore();
	});
});

describe('the typed password never outlives the confirm it was typed for', () => {
	const rule = addRule({ query: 'name:"x"', action: 'hide' });

	async function typed(props: Record<string, unknown> = {}) {
		net.on('GET /filters/preview/jobs/7', { body: finished(weakening()) });
		const flow = createChangeFlow();
		const r = render(ChangeDialog, { props: { flow, title: 'Change', shadowDays: 7, ...props } });
		flow.start(rule);
		await waitFor(() => expect(screen.getByTestId('change-dialog').dataset.state).toBe('confirm'));
		await fireEvent.input(password()!, { target: { value: PW } });
		expect(password()!.value).toBe(PW);
		return { flow, r };
	}
	/** A new preview, then `confirm` again. */
	const confirmAgain = async () => {
		const at = () => screen.getByTestId('change-dialog').dataset.state;
		await waitFor(() => expect(at()).toBe('previewing'));
		await waitFor(() => expect(at()).toBe('confirm'));
	};
	const gone = (flow: ReturnType<typeof createChangeFlow>) => {
		expect(JSON.stringify(flow.state)).not.toContain(PW);
		expect(document.body.innerHTML).not.toContain(PW);
	};

	it('Cancel, then reopening', async () => {
		const { flow } = await typed();
		await fireEvent.click(screen.getByTestId('change-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		flow.start(rule);
		await confirmAgain();
		expect(password()!.value).toBe('');
		gone(flow);
	});

	it('Esc', async () => {
		const { flow } = await typed();
		await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(password()).toBeNull();
		gone(flow);
	});

	it('Shadow for N days (a variant)', async () => {
		const { flow } = await typed();
		await fireEvent.click(screen.getByTestId('change-shadow'));
		await confirmAgain();
		expect(password()!.value).toBe('');
		expect(screen.getByTestId('change-shadow-note')).toBeTruthy();
		gone(flow);
	});

	it('a variant or a new start from outside', async () => {
		for (const next of ['variant', 'start'] as const) {
			const { flow, r } = await typed();
			flow[next]({ ...rule, query: 'name:"y"' });
			await confirmAgain();
			expect(password()!.value).toBe('');
			gone(flow);
			r.unmount();
		}
	});

	it('typed during a rate-limit wait, then closed', async () => {
		net.error('POST /filters/apply', 429, 'rate_limited', {}, { 'Retry-After': '30' });
		const { flow } = await typed();
		await fireEvent.submit(password()!.form!);
		await screen.findByTestId('change-countdown');
		await fireEvent.input(password()!, { target: { value: PW } });
		await fireEvent.submit(password()!.form!);
		expect(net.to('/filters/apply')).toHaveLength(1);
		await fireEvent.click(screen.getByTestId('change-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		gone(flow);
	});

	it('the flow closing on its own still calls onclose', async () => {
		const onclose = vi.fn();
		const { flow } = await typed({ onclose });
		flow.cancel();
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(onclose).toHaveBeenCalledTimes(1);
		gone(flow);
	});
});

describe('the 18+ confirmation', () => {
	it('off needs no dialog; on with Cancel sends nothing; on with confirm sends confirm_adult', async () => {
		net.on('PATCH /settings/content', { body: { revision: 2, values: {} } });
		const adult = createAdultConfirm();
		render(AdultConfirmDialog, { props: { confirm: adult } });

		expect(await setShowAdult(false, adult)).toEqual({ ok: true });
		expect(JSON.parse(net.to('/settings/content')[0]!.body!)).toEqual({ show_adult: false });
		expect(screen.queryByRole('dialog')).toBeNull();

		const cancelled = setShowAdult(true, adult);
		await screen.findByRole('dialog', { name: 'Show adult content?' });
		await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId('adult-cancel')));
		await fireEvent.click(screen.getByTestId('adult-cancel'));
		expect(await cancelled).toEqual({ ok: false });
		expect(net.to('/settings/content')).toHaveLength(1);

		const confirmed = setShowAdult(true, adult);
		await screen.findByRole('dialog');
		await fireEvent.click(await screen.findByTestId('adult-show'));
		expect(await confirmed).toEqual({ ok: true });
		expect(JSON.parse(net.to('/settings/content')[1]!.body!)).toEqual({
			show_adult: true,
			confirm_adult: true
		});
	});

	it('422 adult_confirmation_required is an error and a dev error', async () => {
		const dev = vi.spyOn(console, 'error').mockImplementation(() => {});
		net.error('PATCH /settings/content', 422, 'adult_confirmation_required', {
			field: 'show_adult'
		});
		const adult = createAdultConfirm();
		const r = setShowAdult(true, adult);
		adult.answer(true);
		const res = await r;
		expect(res.ok).toBe(false);
		const { adultErrorText } = await import('./errors');
		if (!res.ok) expect(adultErrorText(res.error!)).toContain("didn't accept");
		expect(dev).toHaveBeenCalledTimes(import.meta.env.DEV ? 1 : 0);
		dev.mockRestore();
	});

	it('409 set_by_environment names the variable', async () => {
		net.error('PATCH /settings/content', 409, 'set_by_environment', {
			field: 'show_adult',
			section: 'content'
		});
		const adult = createAdultConfirm();
		const r = setShowAdult(true, adult);
		adult.answer(true);
		const res = await r;
		expect(res.ok).toBe(false);
		const { adultErrorText } = await import('./errors');
		if (!res.ok) expect(adultErrorText(res.error!)).toContain('LODESTAR_SHOW_ADULT');
	});
});
