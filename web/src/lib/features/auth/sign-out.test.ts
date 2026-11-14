import { beforeEach, describe, expect, it, vi } from 'vitest';
import { toast, toasts } from '$lib/ui/feedback/toasts.svelte';

const session = vi.hoisted(() => ({ logout: vi.fn() }));
vi.mock('./session.svelte', () => session);
import { signOut } from './sign-out';

beforeEach(() => {
	session.logout.mockReset();
	toast.clear();
});

describe('signOut', () => {
	it('shares one request between presses made while it is in flight', async () => {
		let finish!: () => void;
		session.logout.mockReturnValue(new Promise<void>((r) => (finish = r)));
		const first = signOut();
		const second = signOut();
		expect(session.logout).toHaveBeenCalledOnce();
		finish();
		expect(await first).toBe(true);
		expect(await second).toBe(true);
		expect(toasts.items).toEqual([]);
	});

	it('a failure toasts once and lets the next press try again', async () => {
		session.logout.mockRejectedValueOnce(new Error('offline'));
		expect(await signOut()).toBe(false);
		expect(toasts.items.map((t) => t.kind)).toEqual(['error']);
		session.logout.mockResolvedValueOnce(undefined);
		expect(await signOut()).toBe(true);
		expect(session.logout).toHaveBeenCalledTimes(2);
	});
});
