import { beforeEach, describe, expect, it, vi } from 'vitest';

const goto = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto }));

import { toNext } from './nav';

beforeEach(() => goto.mockReset());

describe('toNext', () => {
	it('navigates to next, replacing the sign-in entry', async () => {
		goto.mockResolvedValue(undefined);
		await toNext('/search?q=a');
		expect(goto).toHaveBeenCalledExactlyOnceWith('/search?q=a', { replaceState: true });
	});

	it('falls back to / when the router refuses next, so the page is never stranded', async () => {
		goto.mockRejectedValueOnce(new Error('goto: invalid URL')).mockResolvedValue(undefined);
		await expect(toNext('//evil.example')).resolves.toBeUndefined();
		expect(goto).toHaveBeenLastCalledWith('/', { replaceState: true });
	});
});
