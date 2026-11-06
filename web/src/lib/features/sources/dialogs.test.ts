import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import SourcesScreen from './SourcesScreen.svelte';
import { approved } from './consent-approved.fixture';
import { blocksFor } from './consent';
import { darknet, dht, lime, nyaa, source, sourceList, tracker } from './fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

const GATE =
	"this source asks for your consent before it's turned on, and this version of Lodestar can't show you an approved consent text yet, so it stays off. It can be turned on in a later version";

let net: ReturnType<typeof mockFetch>;
function setup(...list: ReturnType<typeof source>[]) {
	net = mockFetch();
	net.on('GET /sources', { body: sourceList(dht, ...list) });
	return render(SourcesScreen);
}
const writes = () => net.sent.filter((s) => s.method !== 'GET');
const bodyOf = (i = 0) => JSON.parse(writes()[i]!.body!);
const toggleOf = (id: number) =>
	within(screen.getByTestId(`source-${id}`)).getByRole('switch') as HTMLInputElement;
async function open(id: number) {
	await screen.findByTestId(`source-${id}`);
	await fireEvent.click(toggleOf(id));
	return screen.findByRole('dialog');
}
afterEach(() => vi.unstubAllGlobals());

describe('consent dialogs: the approved texts', () => {
	const fill = (
		t: string,
		s: ReturnType<typeof source>,
		vals: Record<string, string | undefined>
	) =>
		t
			.replaceAll('{name}', s.name)
			.replace(/\{(\w+)\}/g, (_, k: string) => vals[k] ?? `{${k}}`)
			.replaceAll('**', '')
			.replace(/\s+/g, ' ')
			.trim();
	const shown = (dlg: HTMLElement) => {
		const title = document.getElementById(dlg.getAttribute('aria-labelledby')!)!.textContent!;
		const text = [...screen.getByTestId('consent-text').children].map((el) =>
			el.tagName === 'UL' ? [...el.children].map((li) => li.textContent!) : [el.textContent!]
		);
		const buttons = [screen.getByTestId('consent-confirm'), screen.getByTestId('consent-cancel')];
		return {
			lines: [title, ...text.flat()].map((t) => t.replace(/\s+/g, ' ').trim()),
			buttons: buttons.map((b) => b.textContent!.replace(/\s+/g, ' ').trim())
		};
	};
	const cases = [
		{
			cls: 'external' as const,
			s: { ...nyaa, endpoints: ['https://nyaa.example/a', 'https://api.nyaa.example/b'] },
			vals: { hosts: 'nyaa.example, api.nyaa.example', egress: 'direct connection' }
		},
		{ cls: 'restricted' as const, s: lime, vals: {} },
		{ cls: 'overlay' as const, s: darknet, vals: { network: 'I2P' } }
	];
	for (const { cls, s, vals } of cases) {
		it(`${cls}: shows exactly the approved text and buttons`, async () => {
			setup(s);
			const dlg = await open(s.id);
			const got = shown(dlg);
			const want = approved[cls];
			expect(got.lines).toEqual(want.lines.map((t) => fill(t, s, vals)));
			expect(got.buttons).toEqual(want.buttons.map((t) => fill(t, s, vals)));
		});
	}

	it('account: the approved text (shown by SRC-15c) is in the messages and matches', () => {
		const b = blocksFor(tracker, 'account');
		const got = [b.title, ...b.paragraphs, b.confirm].map((x) => x.replaceAll('**', ''));
		const want = approved.account;
		expect(got).toEqual(
			[...want.lines, want.buttons[0]].map((x) => fill(x, tracker, { hosts: 'tracker.example' }))
		);
	});

	it('keeps the bold the approved text marks', async () => {
		setup(lime);
		await open(3);
		const bold = [...screen.getByTestId('consent-text').querySelectorAll('strong')].map(
			(b) => b.textContent
		);
		expect(bold).toEqual(['Use a VPN.']);
	});
});

describe('consent dialogs: what is sent', () => {
	it('External: acknowledges with the version the server gave; the server answer decides', async () => {
		const on = { ...nyaa, enabled: true, state: 'enabled' as const, needs: null };
		setup(nyaa);
		net.on('PATCH /sources/{id}', { body: on });
		await open(1);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(bodyOf()).toEqual({ enabled: true, consent: 'external', consent_version: 'v1' });
		expect(toggleOf(1).checked).toBe(true);
	});

	it('shows the consent gate refusal plainly and leaves the source off', async () => {
		setup(nyaa);
		net.on('PATCH /sources/{id}', {
			status: 400,
			body: {
				error: {
					code: 'invalid_setting',
					field: 'enabled',
					message: `The setting wasn't saved: ${GATE}.`
				}
			}
		});
		await open(1);
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		const p = await screen.findByTestId('consent-problem');
		expect(p.textContent!.replace(/\s+/g, ' ')).toContain(
			`The setting wasn't saved: ${GATE}. Nothing was changed.`
		);
		expect(screen.getByRole('dialog')).toBeTruthy();
		expect(toggleOf(1).checked).toBe(false);
		expect(writes()).toHaveLength(1);
	});

	it('Restricted: focus starts on Cancel, the password is required and sent, Escape cancels', async () => {
		setup(lime);
		net.on('PATCH /sources/{id}', { body: { ...lime, enabled: true, needs: null } });
		const dlg = await open(3);
		await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId('consent-cancel')));
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		expect(within(dlg).getByText('Enter your password.')).toBeTruthy();
		expect(writes()).toHaveLength(0);
		const pw = screen.getByTestId('consent-password') as HTMLInputElement;
		expect(pw.type).toBe('password');
		await fireEvent.input(pw, { target: { value: 'hunter2hunter2' } });
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(bodyOf()).toEqual({
			enabled: true,
			consent: 'restricted',
			consent_version: 'v1-ab',
			password: 'hunter2hunter2'
		});
		expect(writes()[0]!.url).not.toContain('hunter2');
	});

	it('a wrong password is said plainly, the field is cleared and the dialog stays', async () => {
		setup(darknet);
		net.error('PATCH /sources/{id}', 401, 'bad_password');
		await open(4);
		const pw = screen.getByTestId('consent-password') as HTMLInputElement;
		await fireEvent.input(pw, { target: { value: 'nope-nope' } });
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		expect((await screen.findByTestId('consent-problem')).textContent).toContain(
			'password is wrong'
		);
		expect(pw.value).toBe('');
		expect(toggleOf(4).checked).toBe(false);
	});

	it('Escape closes without sending anything', async () => {
		setup(darknet);
		const dlg = await open(4);
		await fireEvent.keyDown(dlg, { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(0);
	});
});

describe('consent dialogs: other refusals', () => {
	it('an expired session is not called a wrong password', async () => {
		setup(darknet);
		net.error('PATCH /sources/{id}', 401, 'login_required');
		await open(4);
		await fireEvent.input(screen.getByTestId('consent-password'), {
			target: { value: 'some-pass' }
		});
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		const p = await screen.findByTestId('consent-problem');
		expect(p.textContent).not.toContain('password is wrong');
	});

	it('a changed notice reloads the list so the next try sends the new version', async () => {
		setup(lime);
		net.on('PATCH /sources/{id}', {
			status: 409,
			body: { error: { code: 'consent_required', message: 'x' } }
		});
		await open(3);
		await fireEvent.input(screen.getByTestId('consent-password'), {
			target: { value: 'some-pass' }
		});
		const before = net.sent.filter((s) => s.method === 'GET').length;
		await fireEvent.click(screen.getByTestId('consent-confirm'));
		expect((await screen.findByTestId('consent-problem')).textContent).toContain('changed');
		await waitFor(() =>
			expect(net.sent.filter((s) => s.method === 'GET').length).toBeGreaterThan(before)
		);
	});

	it('Restricted: Escape cancels without sending anything', async () => {
		setup(lime);
		const dlg = await open(3);
		await fireEvent.keyDown(dlg, { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(0);
	});
});

describe('account sources (SRC-15c)', () => {
	it('stay at "needs setup" with the toggle off: no credential dialog on this screen yet', async () => {
		setup(tracker);
		await screen.findByTestId('source-5');
		expect(toggleOf(5).disabled).toBe(true);
		await fireEvent.click(toggleOf(5));
		expect(screen.queryByRole('dialog')).toBeNull();
		expect(writes()).toHaveLength(0);
	});
});

describe('purge', () => {
	const job = (state: string, over: object = {}) => ({
		id: 7,
		kind: 'purge',
		state,
		done: 0,
		skipped: 0,
		total: null,
		total_approx: false,
		eta_s: null,
		rate: null,
		started_at: 1,
		finished_at: state === 'running' ? null : 2,
		purge: {
			kept: 5,
			keys_dropped: 0,
			plugin: 'x',
			snapshots_left: 0,
			snapshots_scrubbed: 0,
			source_id: 1,
			stage: 'removing',
			torrents_deleted: 1204,
			unverified_deleted: 12,
			...over
		}
	});

	it('names what is deleted, starts on Cancel, sends nothing until confirmed', async () => {
		setup(nyaa);
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await screen.findByTestId('purge-text');
		const t = screen.getByTestId('purge-text').textContent!;
		for (const part of [
			'claims',
			'unverified items',
			'credentials',
			'consent record',
			"can't be undone"
		])
			expect(t).toContain(part);
		await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId('purge-cancel')));
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		expect(writes()).toHaveLength(0);
		await fireEvent.click(screen.getByTestId('purge-cancel'));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(writes()).toHaveLength(0);
	});

	it('sends the password, shows progress, then the result', async () => {
		setup(nyaa);
		net.on('POST /sources/{id}/purge', { status: 202, body: job('running') });
		net.on('GET /jobs/{id}', { body: job('running') }, { body: job('done', { stage: 'purged' }) });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'hunter2hunter2' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await screen.findByTestId('purge-stage');
		expect(screen.getByTestId('purge-progress').textContent).toContain('Purging Nyaa');
		const r = await screen.findByTestId('purge-result');
		expect(r.textContent).toBe(
			'1,204 torrents and 12 unverified items deleted; 5 kept because others found them too.'
		);
		expect(bodyOf()).toEqual({ password: 'hunter2hunter2' });
	});

	it('says a wrong password plainly', async () => {
		setup(nyaa);
		net.error('POST /sources/{id}/purge', 401, 'bad_password');
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'bad-bad' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await waitFor(() =>
			expect(screen.getByRole('dialog').textContent).toContain('password is wrong')
		);
	});

	it('says plainly that another purge is running', async () => {
		setup(nyaa);
		net.error('POST /sources/{id}/purge', 409, 'purge_running');
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'pw-pw-pw' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		expect((await screen.findByTestId('purge-problem')).textContent).toContain('already running');
	});

	it('says what is left when the purge is cancelled', async () => {
		setup(nyaa);
		net.on('POST /sources/{id}/purge', { status: 202, body: job('cancelled') });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'pw-pw-pw' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await waitFor(() =>
			expect(screen.getByTestId('purge-progress').textContent).toContain('was cancelled')
		);
	});

	it('says what is left when the purge fails', async () => {
		setup(nyaa);
		net.on('POST /sources/{id}/purge', { status: 202, body: job('failed') });
		await screen.findByTestId('source-1');
		await fireEvent.click(screen.getByTestId('source-1-purge'));
		await fireEvent.input(await screen.findByTestId('purge-password'), {
			target: { value: 'pw-pw-pw' }
		});
		await fireEvent.click(screen.getByTestId('purge-confirm'));
		await waitFor(() =>
			expect(screen.getByTestId('purge-progress').textContent).toContain('stays off')
		);
	});
});
