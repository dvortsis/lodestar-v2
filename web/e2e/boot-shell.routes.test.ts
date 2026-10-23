// The first-paint shell in src/app.html: the page paints from HTML and CSS before any script runs
// (§10.1, LCP), and the app takes it over in place. These specs keep the static markup honest: it
// must be there without JS, match what the app renders box for box (no shift when it is swapped),
// and stay out of the way where it would be wrong (sign-in screens, other languages).
// Name ends in `routes.test.ts` so the `core` project runs it.
import type { Locator, Page } from '@playwright/test';
import { liveEvents } from './support/events-server';
import { expect, test } from './support/test';

// Only the scripts: the stylesheet is what paints the static shell.
const blockApp = (page: Page) => page.route('**/_app/immutable/**/*.js', (route) => route.abort());

interface Box {
	x: number;
	y: number;
	width: number;
	height: number;
}

/** The boxes that must not move: the header, the brand, each destination, the page heading. */
async function boxes(page: Page): Promise<Record<string, Box | null>> {
	const grab = async (loc: Locator) =>
		(await loc.count()) ? await loc.first().boundingBox() : null;
	const out: Record<string, Box | null> = {
		header: await grab(page.locator('header').first()),
		brand: await grab(page.getByRole('link', { name: 'Lodestar, home' })),
		menu: await grab(page.locator('header .x-large\\:hidden').first()),
		h1: await grab(page.locator('main h1').first())
	};
	for (const name of ['Search', 'Discover', 'Alerts', 'Insights', 'Settings']) {
		out[`nav ${name}`] = await grab(page.locator(`header nav a:has-text("${name}")`));
	}
	return out;
}

for (const [label, viewport] of [
	['desktop', { width: 1366, height: 768 }],
	['phone', { width: 390, height: 844 }]
] as const) {
	test.describe(`boot shell at ${label}`, () => {
		test.use({ viewport });

		test('paints with no app script: skip link, bar, links, current page and heading', async ({
			page,
			go
		}) => {
			await blockApp(page);
			await go('/search');
			await expect(page.locator('#boot')).toBeVisible();
			await expect(page.getByRole('link', { name: 'Skip to main content' })).toBeAttached();
			await expect(page.getByRole('link', { name: 'Lodestar, home' })).toBeVisible();
			await expect(page.locator('#boot-h1')).toHaveText('Search');
			await expect(page.locator('#boot-h1')).toBeVisible();
			if (label === 'desktop') {
				const links = page.locator('#boot-header nav a');
				await expect(links).toHaveText(['Search', 'Discover', 'Alerts', 'Insights', 'Settings']);
				await expect(page.locator('#boot-header nav a[aria-current="page"]')).toHaveText('Search');
				for (const a of await links.all()) {
					expect(await a.getAttribute('href')).toMatch(/^\//); // real anchors
				}
			}
		});

		test('marks the current destination on other routes and shows no heading there', async ({
			page,
			go
		}) => {
			await blockApp(page);
			await go('/insights/harvest');
			await expect(page.locator('#boot-h1')).toBeHidden();
			if (label === 'desktop') {
				await expect(page.locator('#boot-header nav a[aria-current="page"]')).toHaveText(
					'Insights'
				);
			}
		});

		test('is replaced by the app in place: same boxes, and #boot is gone', async ({
			page,
			go,
			context
		}) => {
			// 1. The static shell, with the app's scripts blocked.
			const frozen = await context.newPage();
			await blockApp(frozen);
			await frozen.setViewportSize(viewport);
			await frozen.goto('/search');
			await expect(frozen.locator('#boot-h1')).toBeVisible();
			const before = await boxes(frozen);
			await frozen.close();

			// 2. The same page once the app and the full chrome have taken over.
			await liveEvents(page);
			await go('/search');
			await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
			await expect(page.locator('#boot')).toHaveCount(0);
			const after = await boxes(page);

			for (const [name, box] of Object.entries(before)) {
				const now = after[name];
				if (!box) {
					// Hidden by the viewport in the static shell (e.g. the menu button on desktop).
					continue;
				}
				expect(now, `${name} exists after the app mounts`).not.toBeNull();
				for (const key of ['x', 'y', 'width', 'height'] as const) {
					expect(
						Math.abs(box[key] - now![key]),
						`${name}.${key}: static ${box[key]} vs app ${now![key]}`
					).toBeLessThanOrEqual(1);
				}
			}
		});
	});
}

test.describe('where the static shell must not show', () => {
	test('the sign-in and setup screens drop it before first paint', async ({ page, go }) => {
		await blockApp(page);
		for (const path of ['/login', '/setup', '/welcome']) {
			await go(path);
			await expect(page.locator('#boot')).toHaveCount(0);
		}
	});

	test.describe('another language', () => {
		test.use({ locale: 'zh-CN' });
		test('is dropped, so English text never flashes', async ({ page, go }) => {
			await blockApp(page);
			await go('/search');
			await expect(page.locator('#boot')).toHaveCount(0);
		});
	});
});

test('adds no layout shift: CLS stays 0 across the swap', async ({ page, go }) => {
	await page.addInitScript(() => {
		(window as unknown as { __cls: number }).__cls = 0;
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as {
				value: number;
				hadRecentInput: boolean;
			}[]) {
				if (!e.hadRecentInput) (window as unknown as { __cls: number }).__cls += e.value;
			}
		}).observe({ type: 'layout-shift', buffered: true });
	});
	await liveEvents(page);
	await go('/search');
	await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
	expect(await page.evaluate(() => (window as unknown as { __cls: number }).__cls)).toBeLessThan(
		0.001
	);
});
