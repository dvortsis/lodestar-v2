import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/state', async () => ({ page: (await import('./testing.svelte')).testPage }));
const replaceState = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ replaceState }));

import { createKeys } from '$lib/keys';
import { registerSearchCommands, SEARCH_SCOPE } from './commands';
import { ensureSearchPath } from './path';
import { visit } from './testing.svelte';

beforeEach(() => replaceState.mockReset());

describe('ensureSearchPath', () => {
	it('does nothing on /search', () => {
		visit('/search?q=a');
		ensureSearchPath();
		expect(replaceState).not.toHaveBeenCalled();
	});

	it('puts the drawer URL back on /search with the same query, closing the drawer', () => {
		visit('/t/0123456789abcdef0123456789abcdef01234567?q=a+b&sort=-size', { detail: {} });
		ensureSearchPath();
		expect(replaceState).toHaveBeenCalledExactlyOnceWith('/search?q=a+b&sort=-size', {});
	});
});

describe('search commands', () => {
	it('register search.focus on / in the search scope, and clean up', () => {
		const keys = createKeys();
		const focus = vi.fn();
		const off = registerSearchCommands(keys, focus);
		const cmd = keys.list().find((c) => c.id === 'search.focus')!;
		expect(cmd).toMatchObject({ scope: SEARCH_SCOPE, keys: ['/'], active: true });
		expect(cmd.title).toBe('Focus search');
		keys.handle(new KeyboardEvent('keydown', { key: '/' }));
		expect(focus).toHaveBeenCalledOnce();
		off();
		expect(keys.list().find((c) => c.id === 'search.focus')).toBeUndefined();
	});
});
