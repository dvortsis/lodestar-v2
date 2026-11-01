// Advanced (S-updates-2): the channel dialog (Cancel sends nothing, the warning names what changes),
// locked fields, and a refused or failed save putting the server's value back.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import Advanced from './Advanced.svelte';
import { UpdatesStore } from './store.svelte';

const asked = vi.hoisted(() => ({
	answer: 'cancel' as string | null,
	questions: [] as { title: string; body: string[] }[]
}));
vi.mock('../form/dialogs', () => ({
	ask: async (q: { title: string; body: string[] }) => {
		asked.questions.push(q);
		return asked.answer;
	}
}));
vi.mock('$lib/state/events.svelte', () => ({ events: { on: () => () => {} } }));
vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

type F = { value: unknown; env: string | null; read_only: boolean };
let fields: Record<string, F>;
let sent: string[];
let refuse: boolean;
const reply = (code: number, b: unknown) =>
	new Response(JSON.stringify(b), {
		status: code,
		headers: { 'Content-Type': 'application/json' }
	});

beforeEach(() => {
	initLocale('en');
	const f = (value: unknown): F => ({ value, env: null, read_only: false });
	fields = {
		mode: f('notify'),
		level: f('fixes'),
		security_fixes: f('auto'),
		channel: f('stable'),
		wait_days: f(3),
		window: f('02:00-05:00'),
		allow_reindex: f(false),
		notify_before: f(false),
		notify_after: f(false),
		notify_failure: f(true)
	};
	sent = [];
	refuse = false;
	asked.answer = 'cancel';
	asked.questions = [];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			const method = init.method ?? 'GET';
			if (path === '/settings/updates' && method === 'PATCH') {
				sent.push(String(init.body));
				if (refuse)
					return reply(400, {
						error: { code: 'invalid_setting', message: 'The window is not valid.' }
					});
				for (const [k, v] of Object.entries(JSON.parse(String(init.body))))
					if (fields[k]) fields[k].value = v;
				return reply(200, { section: 'updates', revision: 2, restart_required: [], fields });
			}
			if (path === '/settings/updates')
				return reply(200, { section: 'updates', revision: 1, restart_required: [], fields });
			return reply(200, {});
		})
	);
});
afterEach(() => {
	vi.unstubAllGlobals();
	document.body.innerHTML = '';
});

async function open() {
	const store = new UpdatesStore();
	await store.load();
	render(Advanced, { store });
	return store;
}
const radio = (c: string) => screen.getByTestId(`updates-channel-${c}`) as HTMLInputElement;

describe('Advanced', () => {
	it('shows the server’s values', async () => {
		await open();
		expect(radio('stable').checked).toBe(true);
		expect((screen.getByTestId('updates-adv-wait') as HTMLInputElement).value).toBe('3');
		expect((screen.getByTestId('updates-adv-window') as HTMLInputElement).value).toBe(
			'02:00-05:00'
		);
		expect((screen.getByTestId('updates-adv-notify_failure') as HTMLInputElement).checked).toBe(
			true
		);
	});

	it('Cancel on the channel dialog sends nothing and keeps the channel', async () => {
		await open();
		await fireEvent.click(radio('nightly'));
		await until(() => expect(asked.questions).toHaveLength(1));
		await until(() => expect(radio('stable').checked).toBe(true));
		expect(radio('nightly').checked).toBe(false);
		expect(sent).toEqual([]);
	});

	it('says plainly what moving to Beta or Nightly means, then saves on confirm', async () => {
		await open();
		asked.answer = 'switch';
		await fireEvent.click(radio('nightly'));
		await until(() => expect(sent).toHaveLength(1));
		const q = asked.questions[0]!;
		expect(q.title).toBe('Switch to Nightly?');
		expect(q.body.join(' ')).toContain('for testing only');
		expect(q.body.join(' ')).toContain('restore from a snapshot');
		expect(JSON.parse(sent[0]!)).toMatchObject({ channel: 'nightly' });
		await until(() => expect(radio('nightly').checked).toBe(true));
	});

	it('a locked field can’t be changed', async () => {
		fields.channel!.env = 'LODESTAR_UPDATES_CHANNEL';
		fields.allow_reindex!.read_only = true;
		await open();
		expect(radio('beta').disabled).toBe(true);
		expect((screen.getByTestId('updates-adv-reindex') as HTMLInputElement).disabled).toBe(true);
		expect(sent).toEqual([]);
	});

	it('a refused save puts the server’s value back and shows its words', async () => {
		await open();
		const input = screen.getByTestId('updates-adv-window') as HTMLInputElement;
		refuse = true;
		await fireEvent.input(input, { target: { value: '99:99' } });
		await fireEvent.change(input);
		await until(() =>
			expect(screen.getByTestId('updates-adv-reason').textContent).toContain('not valid')
		);
		await until(() => expect(input.value).toBe('02:00-05:00'));
	});

	it('a toggle goes back when the save is refused', async () => {
		await open();
		const t = screen.getByTestId('updates-adv-reindex') as HTMLInputElement;
		refuse = true;
		await fireEvent.click(t);
		await until(() => expect(screen.getByTestId('updates-adv-reason')).toBeTruthy());
		await until(() => expect(t.checked).toBe(false));
	});
});
