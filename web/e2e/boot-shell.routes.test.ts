// The first-paint shell in src/app.html: the page paints from HTML and CSS before any script runs
// (§10.1, LCP), and the app takes it over in place. These specs keep the static markup honest: it
// must be there without JS, match what the app renders box for box (no shift when it is swapped),
// and stay out of the way where it would be wrong (sign-in screens, other languages).
// Name ends in `routes.test.ts` so the `core` project runs it.
import type { Locator, Page } from '@playwright/test';
import { liveEvents } from './support/events-server';
import { interact } from './support/interact';
import { expect, test } from './support/test';

// Only the scripts: the stylesheet is what paints the static shell.
const blockApp = (page: Page) => page.route('**/_app/immutable/**/*.js', (route) => route.abort());

interface Box {
	x: number;
	y: number;
	width: number;
	height: number;
}

/** The boxes that must not move: the header, the brand, each destination, the page heading and
 *  the query bar. */
async function boxes(page: Page): Promise<Record<string, Box | null>> {
	const grab = async (loc: Locator) =>
		(await loc.count()) ? await loc.first().boundingBox() : null;
	const out: Record<string, Box | null> = {
		header: await grab(page.locator('header').first()),
		brand: await grab(page.getByRole('link', { name: 'Lodestar, home' })),
		menu: await grab(page.locator('header .x-large\\:hidden').first()),
		h1: await grab(page.locator('main h1').first()),
		// The search page's query bar is painted statically too (S-search-1).
		bar: await grab(page.locator('main [data-testid="query-editor"]').first())
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
			// Landmarks for a screen-reader user who never presses a key and before any script has run.
			await expect(page.getByRole('banner')).toBeVisible();
			await expect(page.getByRole('main')).toBeVisible();
			await expect(page.locator('#boot-h1 h1')).toHaveText('Search');
			await expect(page.locator('#boot-h1 [data-testid="query-editor"]')).toBeVisible();
			await expect(page.locator('#boot-h1')).toBeVisible();
			if (label === 'desktop') {
				await expect(page.getByRole('navigation', { name: 'Main' })).toBeVisible();
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

	// The app picks a browser language once its catalogue is complete (zh is, since H-zh), so a zh
	// browser gets the Chinese app and the English static shell must not show for it.
	test.describe('another language, auto-selected once complete', () => {
		test.use({ locale: 'zh-CN' });
		test('drops the static shell, because the app will show Chinese', async ({ page, go }) => {
			await blockApp(page);
			await go('/search');
			await expect(page.locator('#boot')).toHaveCount(0);
		});
	});
});

async function cumulativeShift(page: Page) {
	await test.info().attach('layout-shift-sources', {
		body: JSON.stringify(
			await page.evaluate(() => (window as unknown as { __shifts: unknown[] }).__shifts),
			null,
			2
		),
		contentType: 'application/json'
	});
	return page.evaluate(() => (window as unknown as { __cls: number }).__cls);
}
async function watchShift(page: Page) {
	await page.addInitScript(() => {
		const state = window as unknown as { __cls: number; __shifts: unknown[] };
		state.__cls = 0;
		state.__shifts = [];
		new PerformanceObserver((list) => {
			for (const e of list.getEntries() as unknown as {
				value: number;
				startTime: number;
				hadRecentInput: boolean;
				sources: { node?: Element; previousRect: DOMRectReadOnly; currentRect: DOMRectReadOnly }[];
			}[]) {
				if (!e.hadRecentInput) {
					state.__cls += e.value;
					state.__shifts.push({
						value: e.value,
						startTime: e.startTime,
						sources: e.sources.map((source) => ({
							tag: source.node?.nodeName,
							id: source.node?.id,
							className: source.node?.className,
							previous: source.previousRect.toJSON(),
							current: source.currentRect.toJSON()
						}))
					});
				}
			}
		}).observe({ type: 'layout-shift', buffered: true });
	});
}

test('adds no layout shift: CLS stays 0 across the swap', async ({ page, go }) => {
	await watchShift(page);
	await liveEvents(page);
	await go('/search');
	await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
	expect(await cumulativeShift(page)).toBeLessThan(0.001);
});

test('no layout shift on the sign-in and setup screens, where the shell is dropped', async ({
	page,
	go,
	mock
}) => {
	await watchShift(page);
	for (const path of ['/login', '/setup', '/welcome']) {
		// Measure each intended screen, not an auth redirect to another screen.
		mock.on('getAuthStatus', {
			logged_in: path === '/welcome',
			setup_required: path === '/setup'
		});
		await go(path);
		await expect(page.getByRole('main')).toBeVisible();
		await page.waitForLoadState('networkidle');
		expect(new URL(page.url()).pathname).toBe(path);
		expect(await cumulativeShift(page), path).toBeLessThan(0.001);
	}
});

for (const path of ['/login', '/setup']) {
	test(`a delayed auth check reserves the form without exposing its controls at ${path}`, async ({
		page,
		go,
		mock
	}) => {
		let finish!: (value: { logged_in: boolean; setup_required: boolean }) => void;
		const status = new Promise<{ logged_in: boolean; setup_required: boolean }>((resolve) => {
			finish = resolve;
		});
		mock.on('getAuthStatus', () => status);
		await watchShift(page);
		await go(path);
		await expect(page.getByRole('status')).toContainText('Checking');
		await expect(page.locator('main [inert] form')).toHaveCount(1);
		await expect(page.locator('main input:not([hidden])').first()).toBeHidden();
		finish({ logged_in: false, setup_required: path === '/setup' });
		await expect(page.locator('main input:not([hidden])').first()).toBeVisible();
		await expect(page.locator('main [inert]')).toHaveCount(0);
		await page.waitForLoadState('networkidle');
		expect(await cumulativeShift(page)).toBeLessThan(0.001);
	});
}

for (const width of [1280, 390, 320]) {
	test(`setup keeps its layout when the font arrives after the form at ${width}px`, async ({
		page,
		mock
	}) => {
		await page.setViewportSize({ width, height: 844 });
		mock.on('getAuthStatus', { logged_in: false, setup_required: true });
		let releaseFont!: () => void;
		let blockedFonts = 0;
		const fontReady = new Promise<void>((resolve) => (releaseFont = resolve));
		await page.route('**/fonts/*.woff2', async (route) => {
			blockedFonts++;
			await fontReady;
			await route.continue();
		});
		await watchShift(page);
		const painted = () =>
			page.evaluate(
				() =>
					new Promise<void>((resolve) =>
						requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
					)
			);
		try {
			// A font request intentionally remains pending; the load event cannot be our gate.
			await page.goto('/setup', { waitUntil: 'domcontentloaded' });
			await expect(page.locator('main input:not([hidden])').first()).toBeVisible({
				timeout: 10_000
			});
			await expect.poll(() => blockedFonts, { timeout: 10_000 }).toBeGreaterThan(0);
			await painted();
			releaseFont();
			await page.evaluate(() => document.fonts.ready);
			await painted();
			expect(await cumulativeShift(page)).toBeLessThan(0.001);
		} finally {
			releaseFont();
		}
	});
}

test.describe('another language', () => {
	test.use({ locale: 'zh-CN' });
	test('no English flash and no layout shift: the shell and the app agree', async ({
		page,
		go
	}) => {
		await watchShift(page);
		await liveEvents(page);
		await go('/search');
		await expect(page.locator('#boot')).toHaveCount(0);
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
		expect(await cumulativeShift(page)).toBeLessThan(0.001);
	});
});

test.describe('keyboard focus survives the swaps', () => {
	test.use({ viewport: { width: 1366, height: 768 } });
	// Tab N times before the app loads, let it load, wait for the full bar: the same stop is focused.
	for (const [tabs, stop] of [
		[1, 'skip'],
		[2, 'brand'],
		[3, 'nav-search'],
		[5, 'nav-alerts']
	] as const) {
		test(`Tab x${tabs} before load keeps ${stop}`, async ({ page, mock }) => {
			mock.on('listSavedSearches', []);
			let release: () => void = () => {};
			const gate = new Promise<void>((resolve) => (release = resolve));
			await page.route('**/_app/immutable/**/*.js', async (route) => {
				await gate;
				await route.continue();
			});
			await page.goto('/alerts', { waitUntil: 'commit' });
			await expect(page.locator('#boot')).toBeAttached();
			for (let i = 0; i < tabs; i++) await page.keyboard.press('Tab');
			const focused = () =>
				stop === 'skip'
					? page.getByRole('link', { name: 'Skip to main content' })
					: page.locator(`header [data-testid="${stop}"]`);
			await expect(focused()).toBeFocused();
			release();
			await expect(page.locator('#boot')).toHaveCount(0);
			// The full bar replaces the static one a moment later; it must not take the focus with it.
			await expect(page.getByTestId('more-button')).toBeAttached();
			await expect(focused()).toBeFocused();
			// And Tab carries on from there.
			await page.keyboard.press('Tab');
			await expect(focused()).not.toBeFocused();
		});
	}
});

test('the fallback font is never smaller than Atkinson, so the font swap cannot become a later LCP', async ({
	page,
	go
}) => {
	await go('/search');
	await page.evaluate(() => document.fonts.load("16px 'Atkinson Hyperlegible Next'"));
	const worst = await page.evaluate(() => {
		const rect = (family: string, weight: number, px: number, text: string) => {
			const el = document.createElement('span');
			el.textContent = text;
			el.style.cssText = `position:absolute;white-space:nowrap;font:${weight} ${px}px ${family}`;
			document.body.append(el);
			const range = document.createRange();
			range.selectNodeContents(el);
			const box = range.getBoundingClientRect();
			el.remove();
			return box.width * box.height;
		};
		let min = Infinity;
		for (const [weight, px] of [
			[400, 14],
			[400, 16],
			[600, 14],
			[600, 24]
		] as const)
			for (const text of ['Search', 'Discover', 'Alerts', 'Insights', 'Settings']) {
				const ratio =
					rect("'Atkinson Fallback'", weight, px, text) /
					rect("'Atkinson Hyperlegible Next'", weight, px, text);
				min = Math.min(min, ratio);
			}
		return min;
	});
	// A smaller fallback paints a smaller heading first; the swap would then be a larger, later
	// LCP candidate (CI measured 2.67 s against a 1.5 s first paint).
	expect(worst).toBeGreaterThan(1);
});

test('the static links keep the density, so a click before the app loads does not lose it', async ({
	page
}) => {
	await blockApp(page);
	await page.goto('/search?density=compact');
	await expect(page.locator('#boot a[data-boot-nav]').first()).toHaveAttribute(
		'href',
		/\?density=compact$/
	);
	await expect(page.locator('#boot [data-boot-brand]')).toHaveAttribute(
		'href',
		'/search?density=compact'
	);
});

test('Ctrl/Cmd+K as the very first key is taken from the browser, not just replayed', async ({
	page,
	go
}) => {
	await go('/search');
	// The listener is Shell's: it exists once the app has mounted (and taken #boot away).
	await expect(page.locator('#boot')).toHaveCount(0);
	await page.evaluate(() => {
		(window as unknown as { __k: boolean[] }).__k = [];
		window.addEventListener('keydown', (e) => {
			if (e.key.toLowerCase() === 'k')
				(window as unknown as { __k: boolean[] }).__k.push(e.defaultPrevented);
		});
	});
	await page.keyboard.press('Control+k');
	expect(await page.evaluate(() => (window as unknown as { __k: boolean[] }).__k)).toEqual([true]);
	await expect(page.getByRole('dialog')).toBeVisible();
});

test.describe('the error screen when its chunk cannot load', () => {
	// The screen and its parts are the only shared chunks that carry these strings; the stub
	// (nodes/1) with its fallback is left alone.
	const abortErrorView = (page: Page) =>
		page.route('**/_app/immutable/chunks/*.js', async (route) => {
			const response = await route.fetch();
			const body = await response.text();
			if (/state-screen|empty_state_explore/.test(body)) return route.abort();
			return route.fulfill({ response, body });
		});

	test('a missing page still shows the heading and the way home', async ({ page, go }) => {
		await abortErrorView(page);
		await go('/nope-probe');
		await expect(page.getByTestId('state-screen')).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
		await expect(page.getByRole('link', { name: 'Go to Search' })).toBeVisible();
	});
	test('a failed navigation shows the error, the code and both actions', async ({ page, go }) => {
		await go('/search');
		await interact(page); // the full bar loads on the first interaction
		await expect(page.getByTestId('more-button')).toBeAttached();
		await abortErrorView(page);
		// The destination's own chunk fails too, as it does when the server has gone.
		await page.route('**/_app/immutable/nodes/*.js', (route) => route.abort());
		await page.getByRole('link', { name: 'Discover' }).first().click();
		await expect(page.getByRole('heading', { name: 'Something went wrong' })).toBeVisible();
		await expect(page.getByText('Error 500')).toBeVisible();
		await expect(page.getByRole('link', { name: 'Go to Search' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
	});
});
