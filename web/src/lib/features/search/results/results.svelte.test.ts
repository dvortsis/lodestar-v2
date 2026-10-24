// The store inside an effect, as the list uses it (runes need a `.svelte` test file).
import { flushSync } from 'svelte';
import { expect, it, vi } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { page } from './fixtures';

const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { post }
}));

const { createResults } = await import('./results.svelte');

it.each([
	['no matches', []],
	['matches', [1, 2]]
])('an answer with %s does not load the query again', async (_, ids) => {
	post.mockReset();
	post.mockResolvedValue(page(ids));
	const r = createResults();
	const q = {
		ast: { op: 'word' } as unknown as Ast,
		sort: 'relevance',
		hidden: false,
		exact: false
	};
	const stop = $effect.root(() => {
		$effect(() => void r.load(q));
	});
	flushSync();
	await vi.waitFor(() => expect(r.status).toBe('ready'), { timeout: 10_000 });
	flushSync();
	await Promise.resolve();
	expect(post).toHaveBeenCalledTimes(1);
	stop();
});
