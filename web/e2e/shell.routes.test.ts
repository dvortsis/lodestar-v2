// The app shell (architecture §10.3, §10.13, §10.19): navigation and its responsive collapse, the
// status pill, the command palette, the full-page states and the 401 hand-off. Keyboard-only flows
// are in shell.keyboard.test.ts. Name ends in `routes.test.ts` so the `core` project runs it.
import { expectNoA11yViolations } from './a11y';
import { crawlerEvent, engage, liveEvents, openPaletteWithKey } from './support/events-server';
import { expect, test } from './support/test';
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';

const nav = (page: import('@playwright/test').Page) =>
	page.getByRole('navigation', { name: 'Main' });

test.describe('navigation', () => {
	test('shows five destinations and marks the current one', async ({ page, go }) => {
		await go('/insights/harvest');
		const links = nav(page).getByRole('link');
		await expect(links).toHaveText(['Search', 'Discover', 'Alerts', 'Insights', 'Settings']);
		await expect(nav(page).getByRole('link', { name: 'Insights' })).toHaveAttribute(
			'aria-current',
			'page'
		);
		await expect(nav(page).getByRole('link', { name: 'Search' })).not.toHaveAttribute(
			'aria-current'
		);
	});

	test('has the landmarks and a heading in main', async ({ page, go }) => {
		await go('/alerts');
		await expect(page.getByRole('banner')).toBeVisible();
		await expect(page.getByRole('main')).toBeVisible();
		await expect(page.getByRole('main').getByRole('heading', { level: 1 })).toBeVisible();
	});

	test('keeps the density in the URL when moving between destinations', async ({ page }) => {
		await page.goto('/search?density=compact');
		await nav(page).getByRole('link', { name: 'Alerts' }).click();
		await expect(page).toHaveURL(/\/alerts\?density=compact$/);
	});

	test('the logo goes home', async ({ page, go }) => {
		await go('/alerts');
		await page.getByRole('link', { name: 'Lodestar, home' }).click();
		await expect(page).toHaveURL(/\/search/);
	});

	test('the overflow menu lists its five items and opens a destination', async ({ page, go }) => {
		await go('/search');
		await engage(page); // the full bar loads on the first interaction
		await page.getByTestId('more-button').click();
		const menu = page.getByRole('menu', { name: 'More' });
		await expect(menu.getByRole('menuitem')).toHaveText([
			'Import',
			'Backup',
			'Logs',
			'Syntax guide',
			'About'
		]);
		await menu.getByRole('menuitem', { name: 'About' }).click();
		await expect(page).toHaveURL(/\/settings\/about/);
	});
});

test.describe('responsive navigation', () => {
	test('shows the links inline from 1024 px', async ({ page, go }) => {
		await page.setViewportSize({ width: 1024, height: 768 });
		await go('/search');
		await expect(nav(page).getByRole('link', { name: 'Alerts' })).toBeVisible();
		await expect(page.getByTestId('menu-button')).toBeHidden();
	});

	for (const width of [1023, 640, 390, 320]) {
		test(`collapses behind a menu button at ${width} px`, async ({ page, go }) => {
			await page.setViewportSize({ width, height: 800 });
			await go('/search');
			await expect(nav(page)).toHaveCount(0);
			await engage(page);
			const button = page.getByTestId('menu-button');
			await expect(button).toHaveAttribute('aria-expanded', 'false');
			await button.click();
			await expect(button).toHaveAttribute('aria-expanded', 'true');
			await expect(nav(page).getByRole('link')).toHaveCount(5);
			await nav(page).getByRole('link', { name: 'Discover' }).click();
			await expect(page).toHaveURL(/\/discover/);
			await expect(nav(page)).toHaveCount(0); // closes after choosing
			// No horizontal scroll at any width.
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
				width
			);
		});
	}

	test('a tap on the static menu button before the bar has loaded opens the menu', async ({
		page,
		go
	}) => {
		await page.setViewportSize({ width: 390, height: 800 });
		await go('/search');
		// The static bar from app.html is still in place: its menu button is inert markup.
		await expect(page.getByTestId('menu-button')).toHaveCount(0);
		await page.locator('header .x-large\\:hidden').first().click();
		await expect(page.getByTestId('menu-button')).toHaveAttribute('aria-expanded', 'true');
		await expect(nav(page).getByRole('link')).toHaveCount(5);
	});

	test('the status pill keeps its words for assistive tech when only the icon shows', async ({
		page,
		go
	}) => {
		await liveEvents(page);
		await page.setViewportSize({ width: 390, height: 800 });
		await go('/search');
		const pill = page.getByRole('link', { name: /Crawling/ });
		await expect(pill).toBeVisible();
		await expect(pill).toContainText('Crawler status. Open Insights.');
		// Visually hidden: clipped to a pixel, still in the accessibility tree.
		const box = await pill.locator('span.sr-only').first().boundingBox();
		expect(box!.width).toBeLessThanOrEqual(1);
	});
});

test.describe('status pill', () => {
	test('shows the crawler state and the server count, and follows the stream', async ({
		page,
		go
	}) => {
		const events = await liveEvents(page);
		await go('/search');
		const pill = page.getByTestId('status-pill');
		await expect(pill).toHaveAttribute('data-state', 'crawling');
		await expect(page.getByTestId('status-pill-text')).toHaveText('Crawling');
		await expect(page.getByTestId('status-pill-count')).toHaveText('· 1.2M torrents');
		await expect(pill).toHaveAttribute('href', '/insights');

		events.send('crawler', crawlerEvent({ fetch_active: 0, fetch_pending: 0, torrents: 0 }));
		await expect(pill).toHaveAttribute('data-state', 'idle');
		await expect(page.getByTestId('status-pill-count')).toHaveText('· 0 torrents');

		events.send('rebuild', {
			done: 1500,
			error: null,
			from: 'v3',
			phase: 'building',
			to: 'v4',
			total: 20000,
			unavailable: []
		});
		await expect(pill).toHaveAttribute('data-state', 'rebuilding');
		await expect(page.getByTestId('status-pill-count')).toHaveText('· 1,500 of 20,000');
	});

	test('asks the stream for the topics it shows', async ({ page, go }) => {
		const events = await liveEvents(page);
		await go('/search');
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
		expect((await events.requests())[0]).toContain('topics=crawler%2Crebuild');
	});

	test('reads "Connecting" until the stream answers', async ({ page, go }) => {
		await page.route('**/api/v1/events*', () => new Promise(() => {}));
		await go('/search');
		await engage(page);
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'connecting');
		await expect(page.getByTestId('status-pill-text')).toHaveText('Connecting');
	});

	test('the banner and the unreachable page need no lazy chunk, so they show when the server is gone', async ({
		page,
		go
	}) => {
		const events = await liveEvents(page);
		await go('/search');
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
		await page.route('**/_app/immutable/**', (route) => route.abort('connectionrefused'));
		await events.drop();
		await expect(page.getByTestId('reconnecting-banner')).toContainText('Lost contact');
		await expect(page.getByRole('heading', { name: 'Search' })).toBeVisible();
	});

	test('a stream that ends keeps the page and shows a banner and "Reconnecting"', async ({
		page,
		go
	}) => {
		const events = await liveEvents(page);
		await go('/search');
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
		events.drop();
		await expect(page.getByTestId('reconnecting-banner')).toContainText('Lost contact');
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'reconnecting');
		await expect(page.getByRole('heading', { name: 'Search' })).toBeVisible();
		// ...and it recovers by itself.
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling', {
			timeout: 15_000
		});
		await expect(page.getByTestId('reconnecting-banner')).toHaveCount(0);
	});
});

test.describe('the chrome loads on the first interaction', () => {
	test('a visitor who never interacts gets the full bar after 30 s', async ({ page, go, mock }) => {
		await page.clock.install();
		await go('/search');
		await expect(page.locator('#boot')).toHaveCount(0);
		await page.clock.runFor(29_000);
		await expect(page.getByTestId('palette-button')).toHaveCount(0);
		await page.clock.runFor(1_500);
		await expect(page.getByTestId('palette-button')).toBeAttached();
		await expect.poll(() => mock.calls('getEvents').length).toBe(1);
	});

	test('one key loads it and starts the stream: no second interaction needed', async ({
		page,
		go,
		mock
	}) => {
		await page.clock.install();
		await go('/search');
		await expect(page.locator('#boot')).toHaveCount(0); // the app has mounted
		await page.clock.runFor(5_000); // well past load, still short of the 30 s fallback
		// Nothing yet: the static bar stays and no stream is open (the bare page's budget, §10.1).
		await expect(page.getByTestId('palette-button')).toHaveCount(0);
		expect(mock.calls('getEvents')).toHaveLength(0);
		await page.keyboard.press('Escape');
		await expect(page.getByTestId('palette-button')).toBeAttached();
		await expect.poll(() => mock.calls('getEvents').length).toBe(1);
	});
});

test.describe('command palette', () => {
	test('opens from the button, filters, runs a command and closes', async ({ page, go, mock }) => {
		mock.on('getInsightsOverview', overviewFixture());
		mock.on('getCrawler', crawlerFixture());
		await go('/search');
		await engage(page);
		await page.getByTestId('palette-button').click();
		const dialog = page.getByRole('dialog', { name: 'Command palette' });
		const input = dialog.getByRole('combobox', { name: 'Type a command' });
		await expect(input).toBeFocused();
		await expect(dialog.getByRole('option', { name: /Go to Alerts/ })).toBeVisible();
		await input.fill('insi');
		await expect(dialog.getByRole('option')).toHaveCount(1);
		await input.press('Enter');
		await expect(page).toHaveURL(/\/insights/);
		await expect(dialog).toBeHidden();
	});

	test('opens with Ctrl+K even while typing in a field, and toggles closed', async ({
		page,
		go
	}) => {
		await liveEvents(page);
		await go('/search');
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling'); // idle work done
		await page.evaluate(() => {
			const input = document.createElement('input');
			input.setAttribute('aria-label', 'scratch');
			document.getElementById('main')!.append(input);
			input.focus();
		});
		await openPaletteWithKey(page, () => page.keyboard.press('Control+k'));
		const dialog = page.getByRole('dialog', { name: 'Command palette' });
		await page.keyboard.press('Control+k');
		await expect(dialog).toBeHidden();
	});

	test('says so when nothing matches', async ({ page, go }) => {
		await go('/search');
		await engage(page);
		await page.getByTestId('palette-button').click();
		await page.getByRole('combobox', { name: 'Type a command' }).fill('zzzz');
		await expect(page.getByText('No command matches “zzzz”')).toBeVisible();
	});
});

test.describe('full-page states', () => {
	test('an unknown address shows the not-found state with the way out first', async ({
		page,
		go
	}) => {
		await go('/definitely-not-a-page');
		await expect(page.getByRole('heading', { level: 1, name: 'Page not found' })).toBeVisible();
		const link = page.getByRole('link', { name: 'Go to Search' });
		await expect(link).toBeVisible();
		// Copy and the next step come before the (decorative) illustration in reading order.
		const order = await page.evaluate(() => {
			const s = document.querySelector('[data-testid="state-screen"]')!;
			const h = s.querySelector('h1')!;
			const img = s.querySelector('img');
			return !img || !!(h.compareDocumentPosition(img) & Node.DOCUMENT_POSITION_FOLLOWING);
		});
		expect(order).toBe(true);
		await link.click();
		await expect(page).toHaveURL(/\/search/);
	});

	test('the illustration is decorative, lazy and sized', async ({ page, go }) => {
		await page.setViewportSize({ width: 1366, height: 800 });
		await go('/definitely-not-a-page');
		const img = page.locator('[data-testid="state-screen"] img');
		await expect(img).toHaveAttribute('alt', '');
		await expect(img).toHaveAttribute('loading', 'lazy');
		await expect(img).toHaveAttribute('width', /\d+/);
		await expect(img).toHaveAttribute('height', /\d+/);
	});

	test('the illustration is hidden when the viewport is shorter than 600 px', async ({
		page,
		go
	}) => {
		await page.setViewportSize({ width: 390, height: 560 });
		await go('/definitely-not-a-page');
		await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
		await expect(page.locator('[data-testid="state-screen"] img')).toBeHidden();
	});

	test('a server that never answers shows the unreachable page, and Try again reloads', async ({
		page,
		go
	}) => {
		await page.route('**/api/v1/events*', (route) => route.abort('connectionrefused'));
		await page.route('**/api/v1/auth', (route) => route.abort('connectionrefused'));
		await go('/search');
		await engage(page);
		await expect(
			page.getByRole('heading', { level: 1, name: 'Can’t reach Lodestar' })
		).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Search' })).toHaveCount(0);
		// The top bar stays, so the person can still see where they are.
		await expect(page.getByRole('banner')).toBeVisible();
		await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
		await page.unroute('**/api/v1/events*');
		await liveEvents(page);
		await page.getByRole('button', { name: 'Try again' }).click();
		await expect(page.getByRole('heading', { name: 'Search' })).toBeVisible();
	});

	test('a blocked event stream does not lock anyone out while the rest of the API answers', async ({
		page,
		go,
		mock
	}) => {
		mock.on('getAuthStatus', { logged_in: true, setup_required: false });
		await page.route('**/api/v1/events*', (route) => route.abort('connectionrefused'));
		await go('/search');
		await engage(page);
		await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'reconnecting');
		await expect(page.getByRole('heading', { name: 'Search' })).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Can’t reach Lodestar' })).toHaveCount(0);
	});
});

test.describe('401 hand-off', () => {
	const unauthorized = (setup: boolean) => ({
		status: 401,
		contentType: 'application/json',
		body: JSON.stringify({
			error: { code: 'login_required', message: 'Sign in first.' },
			setup_required: setup
		})
	});

	test('a first run goes to /setup', async ({ page, mock }) => {
		// The setup screen checks GET /auth itself and goes back to /login if no setup is needed.
		mock.on('getAuthStatus', { logged_in: false, setup_required: true });
		await page.route('**/api/v1/events*', (route) => route.fulfill(unauthorized(true)));
		await page.goto('/search');
		await engage(page);
		await expect(page).toHaveURL(/\/setup$/);
	});

	test('a signed-out visit goes to /login and remembers where it was going', async ({ page }) => {
		await page.route('**/api/v1/events*', (route) => route.fulfill(unauthorized(false)));
		await page.goto('/alerts?x=1');
		await engage(page);
		await expect(page).toHaveURL(/\/login\?next=%2Falerts%3Fx%3D1$/);
	});

	test('the sign-in and setup screens have no chrome and open no stream', async ({
		page,
		go,
		mock
	}) => {
		const events = await liveEvents(page);
		for (const path of ['/login', '/setup', '/welcome']) {
			await go(path);
			await engage(page); // the stream starts on the first interaction: none must on these screens
			await expect(page.getByRole('main')).toBeVisible();
			await expect(page.getByRole('banner')).toHaveCount(0);
			await expect(page.getByRole('navigation')).toHaveCount(0);
			await page.waitForTimeout(1500); // the shell's idle work would have started by now
			expect(await events.requests()).toHaveLength(0);
		}
		expect(mock.calls('getEvents')).toHaveLength(0);
	});
});

test.describe('skip link', () => {
	test('is the first stop, hidden until focused, and moves focus to main', async ({ page, go }) => {
		await go('/alerts');
		const skip = page.getByRole('link', { name: 'Skip to main content' });
		// It is in the first paint, before the chrome (top bar) has loaded.
		await expect(skip).toBeAttached();
		await page.keyboard.press('Tab');
		await expect(skip).toBeFocused();
		await expect(skip).toBeInViewport();
		await page.keyboard.press('Enter');
		await expect(page.locator('main')).toBeFocused();
		await expect(page).toHaveURL(/\/alerts(\?|$)/);
	});
});

for (const [name, themeId, colorMode] of [
	['Riverstone light', 'riverstone-light', 'light'],
	['Riverstone dark', 'riverstone-dark', 'dark']
] as const) {
	test.describe(`axe, ${name}`, () => {
		test.use({ themeId, colorMode });

		for (const width of [1366, 390]) {
			test(`the shell at ${width} px`, async ({ page, go }) => {
				await liveEvents(page);
				await page.setViewportSize({ width, height: 800 });
				await go('/search');
				await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
				await expectNoA11yViolations(page);
			});
		}

		test('the open mobile menu', async ({ page, go }) => {
			await page.setViewportSize({ width: 390, height: 800 });
			await go('/search');
			await engage(page);
			await page.getByTestId('menu-button').click();
			await expect(nav(page)).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('the open palette', async ({ page, go }) => {
			await go('/search');
			await engage(page);
			await page.getByTestId('palette-button').click();
			await expect(page.getByRole('option', { name: /Go to Alerts/ })).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('the open overflow menu', async ({ page, go }) => {
			await go('/search');
			await engage(page);
			await page.getByTestId('more-button').click();
			await expect(page.getByRole('menuitem', { name: 'About' })).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('the not-found page', async ({ page, go }) => {
			await go('/definitely-not-a-page');
			await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('the unreachable page', async ({ page, go }) => {
			await page.route('**/api/v1/events*', (route) => route.abort('connectionrefused'));
			await page.route('**/api/v1/auth', (route) => route.abort('connectionrefused'));
			await go('/search');
			await engage(page);
			await expect(page.getByRole('heading', { name: 'Can’t reach Lodestar' })).toBeVisible();
			await expectNoA11yViolations(page);
		});

		test('the reconnecting banner', async ({ page, go }) => {
			const events = await liveEvents(page);
			await go('/search');
			await expect(page.getByTestId('status-pill')).toHaveAttribute('data-state', 'crawling');
			events.drop();
			await expect(page.getByTestId('reconnecting-banner')).toBeVisible();
			await expectNoA11yViolations(page);
		});
	});
}
