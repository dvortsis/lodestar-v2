// The settled page a smoke or all-themes check scans and photographs: a still skeleton passes the
// snapshot's stability check (two identical captures, animations disabled), so the shot must
// wait for the page's content, not for its pixels to stop moving.
import type { Page } from '@playwright/test';
import type { RouteCase } from './routes';
import { expect } from './test';

/** Every loading placeholder on the page: a busy region or a skeleton. None is left once settled. */
const placeholders = (page: Page) => page.locator('[aria-busy="true"], [data-testid="skeleton"]');

/**
 * Waits for the route's heading, then its own `ready` condition, then for every loading
 * placeholder to go, each with a deadline of at least 10 s.
 */
export async function settle(page: Page, route: Pick<RouteCase, 'heading' | 'ready'>) {
	if (route.heading)
		await expect(page.getByRole('heading', { name: route.heading })).toBeVisible({
			timeout: 10_000
		});
	await route.ready?.(page);
	await expect(placeholders(page)).toHaveCount(0, { timeout: 10_000 });
}
