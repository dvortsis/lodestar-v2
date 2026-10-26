import { expect, it, vi } from 'vitest';
import { cacheDetail, cachedDetail, detailGeneration } from './cache';
import { HASH, torrentDetail } from './fixtures';
const on = vi.hoisted(() => vi.fn());
vi.mock('$lib/state/events.svelte', () => ({ events: { on } }));
import { watchDetailVisibility } from './visibility';

it('subscribes once and invalidates for content settings from any tab and every resync', () => {
	watchDetailVisibility();
	watchDetailVisibility();
	expect(on).toHaveBeenCalledTimes(1);
	const [topic, received, options] = on.mock.calls[0];
	expect(topic).toBe('settings');
	cacheDetail(HASH, torrentDetail());
	received({ section: 'network' });
	expect(cachedDetail(HASH)).toBeDefined();
	const generation = detailGeneration();
	received({ section: 'content' });
	expect(cachedDetail(HASH)).toBeUndefined();
	expect(detailGeneration()).toBeGreaterThan(generation);
	cacheDetail(HASH, torrentDetail());
	options.onResync();
	expect(cachedDetail(HASH)).toBeUndefined();
});
