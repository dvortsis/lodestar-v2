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

import { KEPT_KEY } from './draft-scope';
import type { SettingsForm } from './form.svelte';
import { ask } from './dialogs';
import { askToLeave } from './questions';
import { askSignedOut } from './signed-out';
import { spec } from './testing/values';
import type { FieldSpec, SaveResult } from './types';

type Fake = Pick<
	SettingsForm,
	'id' | 'dirtyCount' | 'save' | 'settled' | 'discard' | 'keepForSignIn' | 'specs' | 'field'
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
		keepForSignIn: vi.fn(() => true),
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

describe('askSignedOut', () => {
	const login = new URL('/login?next=%2Fsettings%2Fcrawler', location.origin);

	it('names the Legal Notice when that is what is due', async () => {
		dialogs.answer = 'keep';
		await askSignedOut(ask, fakeForm([]), login, 'notice', signal, vi.fn(), vi.fn());
		const q = JSON.stringify(dialogs.asked);
		expect(q).toContain('Accept the Legal Notice to save');
		expect(q).toContain('Continue');
	});

	it('keeps the draft and goes to sign in', async () => {
		dialogs.answer = 'signin';
		const form = fakeForm([]);
		const navigate = vi.fn(async () => {});
		const stay = vi.fn();
		await askSignedOut(ask, form, login, 'expired', signal, navigate, stay);
		expect(form.keepForSignIn).toHaveBeenCalledOnce();
		expect(sessionStorage.getItem(KEPT_KEY)).toContain(location.pathname);
		expect(navigate).toHaveBeenCalledWith(login.href);
		expect(stay).not.toHaveBeenCalled();
		expect(JSON.stringify(dialogs.asked)).toContain('This browser tab keeps them meanwhile');
	});

	it.each(['keep', null])('stays on %s, dropping the marker', async (answer) => {
		dialogs.answer = answer;
		sessionStorage.setItem(KEPT_KEY, '{}');
		const form = fakeForm([]);
		const navigate = vi.fn(async () => {});
		const stay = vi.fn();
		await askSignedOut(ask, form, login, 'expired', signal, navigate, stay);
		expect(navigate).not.toHaveBeenCalled();
		expect(stay).toHaveBeenCalledOnce();
		expect(sessionStorage.getItem(KEPT_KEY)).toBeNull();
	});

	it('never promises to keep what it can’t', async () => {
		dialogs.answer = 'keep';
		await askSignedOut(
			ask,
			fakeForm([], { keepForSignIn: vi.fn(() => false) }),
			login,
			'expired',
			signal,
			vi.fn(),
			vi.fn()
		);
		expect(JSON.stringify(dialogs.asked)).toContain("they'll be lost");
	});

	it('says passwords and keys are never kept', async () => {
		dialogs.answer = 'keep';
		const secret = spec('notifications.ntfy_token', { writeOnly: true });
		await askSignedOut(
			ask,
			fakeForm([], {
				dirtyCount: 2,
				specs: [secret],
				field: () => ({ dirty: true }) as never
			}),
			login,
			'expired',
			signal,
			vi.fn(),
			vi.fn()
		);
		expect(JSON.stringify(dialogs.asked)).toContain("Passwords and keys aren't kept");
	});
});
