// The leave dialog's "Save changes" (UI review L2) and the signed-out question
// (X-session-draft-keep), with the dialog answered by a stub.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const dialogs = vi.hoisted(() => ({ answer: 'save' as string | null, asked: [] as unknown[] }));
vi.mock('./dialogs', () => ({
	ask: async (q: unknown) => {
		dialogs.asked.push(q);
		return dialogs.answer;
	}
}));

import { DRAFT_PREFIX, KEPT_KEY, REARM_EVENT, SIGN_IN_KEY } from './draft-scope';
import type { SettingsForm } from './form.svelte';
import { askToLeave } from './questions';
import { onSessionEnd } from './signed-out';
import { spec } from './testing/values';
import type { FieldSpec, SaveResult } from './types';

type Fake = Pick<
	SettingsForm,
	'id' | 'dirtyCount' | 'save' | 'settled' | 'discard' | 'specs' | 'field'
>;

type FakeForm = Fake & { saves: number } & SettingsForm;

function fakeForm(results: SaveResult[], over: Partial<Fake> = {}): FakeForm {
	let saves = 0;
	const f: Fake & { saves: number } = {
		get saves() {
			return saves;
		},
		id: 'crawler',
		dirtyCount: 1,
		specs: [] as FieldSpec[],
		field: () => ({ dirty: false }) as never,
		save: vi.fn(async (): Promise<SaveResult> => results[saves++] ?? { ok: true, restart: [] }),
		settled: vi.fn(async () => {}),
		discard: vi.fn(),
		...over
	};
	return f as FakeForm;
}

const to = new URL('/search', location.origin);
const signal = new AbortController().signal;

beforeEach(() => {
	dialogs.answer = 'save';
	dialogs.asked = [];
	sessionStorage.clear();
});

describe('askToLeave: Save changes', () => {
	it('waits for a save already in flight, then saves what is left and leaves', async () => {
		const form = fakeForm([
			{ ok: false, reason: 'busy' },
			{ ok: true, restart: [] }
		]);
		const navigate = vi.fn(async () => {});
		await askToLeave(form, to, false, signal, navigate);
		expect(form.settled).toHaveBeenCalledOnce();
		expect(form.save).toHaveBeenCalledTimes(2);
		expect(navigate).toHaveBeenCalledWith(to.href);
	});

	it('stays on the page when the save fails', async () => {
		const form = fakeForm([{ ok: false, reason: 'refused' }]);
		const navigate = vi.fn(async () => {});
		await askToLeave(form, to, false, signal, navigate);
		expect(navigate).not.toHaveBeenCalled();
	});

	it('leaves when nothing is left to save', async () => {
		const form = fakeForm([{ ok: false, reason: 'nothing' }]);
		const navigate = vi.fn(async () => {});
		await askToLeave(form, to, false, signal, navigate);
		expect(navigate).toHaveBeenCalledOnce();
	});
});

describe('onSessionEnd (signed-out.ts)', () => {
	const login = new URL('/login?next=%2Fsettings%2Fcrawler', location.origin);

	/** A form as the engine is: it writes a draft for its dirty fields when asked. */
	function engine(over: Record<string, unknown> = {}) {
		const f = {
			...fakeForm([]),
			useDrafts: true,
			draftUnavailable: false,
			status: 'ready',
			draft: null,
			draftData: null,
			writeNow: vi.fn(() => sessionStorage.setItem(DRAFT_PREFIX + 'crawler', '{"v":1}')),
			...over
		};
		return f as unknown as SettingsForm;
	}

	it('keeps the draft, marks it for this screen and goes to sign in', async () => {
		dialogs.answer = 'signin';
		const navigate = vi.fn(async () => {});
		await onSessionEnd('signin', engine(), login, signal, navigate, vi.fn());
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'crawler')).not.toBeNull();
		expect(JSON.parse(sessionStorage.getItem(KEPT_KEY)!)).toMatchObject({
			id: 'crawler',
			path: location.pathname
		});
		expect(navigate).toHaveBeenCalledWith(login.href);
		expect(JSON.stringify(dialogs.asked)).toContain("You've been signed out");
		expect(JSON.stringify(dialogs.asked)).toContain('This browser tab keeps them meanwhile');
	});

	it.each(['keep', null])(
		'stays on %s: drops the mark and re-arms the redirect',
		async (answer) => {
			dialogs.answer = answer;
			const rearm = vi.fn();
			addEventListener(REARM_EVENT, rearm);
			const navigate = vi.fn(async () => {});
			await onSessionEnd('signin', engine(), login, signal, navigate, vi.fn());
			removeEventListener(REARM_EVENT, rearm);
			expect(navigate).not.toHaveBeenCalled();
			expect(rearm).toHaveBeenCalledOnce();
			expect(sessionStorage.getItem(KEPT_KEY)).toBeNull();
		}
	);

	it('never promises to keep what it can’t', async () => {
		dialogs.answer = 'keep';
		await onSessionEnd('signin', engine({ useDrafts: false }), login, signal, vi.fn(), vi.fn());
		expect(JSON.stringify(dialogs.asked)).toContain("they'll be lost");
	});

	it('says passwords, keys, secret links and client entries are never kept', async () => {
		dialogs.answer = 'keep';
		const secret = spec('notifications.ntfy_token', { writeOnly: true });
		await onSessionEnd(
			'signin',
			engine({ dirtyCount: 2, specs: [secret], field: () => ({ dirty: true }) }),
			login,
			signal,
			vi.fn(),
			vi.fn()
		);
		expect(JSON.stringify(dialogs.asked)).toContain(
			'Passwords, keys, secret links and download-client entries'
		);
	});

	it('names the Legal Notice when that is what is due', async () => {
		dialogs.answer = 'keep';
		sessionStorage.setItem(SIGN_IN_KEY, 'notice');
		await onSessionEnd('signin', engine(), login, signal, vi.fn(), vi.fn());
		const q = JSON.stringify(dialogs.asked);
		expect(q).toContain('Accept the Legal Notice to save');
		expect(q).toContain('Continue');
	});

	it('goes straight on after a Sign out or to setup, keeping nothing', async () => {
		const form = engine();
		const navigate = vi.fn(async () => {});
		await onSessionEnd(
			'setup',
			form,
			new URL('/setup', location.origin),
			signal,
			navigate,
			vi.fn()
		);
		expect(navigate).toHaveBeenCalledOnce();
		expect(dialogs.asked).toHaveLength(0);
	});
});
