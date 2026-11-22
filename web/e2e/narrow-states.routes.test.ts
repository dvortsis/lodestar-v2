// X-narrow-states: Logs on a phone (wrapped rows, labelled filters, no layout shift in the list),
// Discover without its unbuilt sections, and the tab strip's scroll cue, at 390 px against the mock.
import { expectNoA11yViolations } from './a11y';
import { liveLogs, logLine } from './logs.stream';
import { expect as baseExpect, test } from './support/test';
import {
	discoverResponse,
	hit,
	newest,
	response,
	showAdultSettings
} from '../src/lib/features/discover/testing';

const expect = baseExpect.configure({ timeout: 10_000 });
test.use({ viewport: { width: 390, height: 844 } });

const LONG =
	'A long message that has to wrap on a phone instead of showing four characters. '.repeat(3);

const history = (lines: ReturnType<typeof logLine>[]) => ({
	lines: lines.toReversed(),
	more: false,
	next_cursor: null,
	unreadable: 0
});

declare global {
	interface Window {
		__logShifts?: number[];
	}
}

test.describe('Logs at 390 px', () => {
	test('wraps the message by default, labels the filters and never shifts the list', async ({
		page,
		go,
		mock
	}) => {
		// Count layout shifts whose source is inside the log list.
		await page.addInitScript(() => {
			window.__logShifts = [];
			new PerformanceObserver((list) => {
				for (const e of list.getEntries() as unknown as {
					value: number;
					hadRecentInput: boolean;
					sources: { node: Node | null }[];
				}[]) {
					if (e.hadRecentInput) continue;
					for (const s of e.sources) {
						const el = s.node instanceof Element ? s.node : s.node?.parentElement;
						if (el?.closest('[role="log"]')) window.__logShifts!.push(e.value);
					}
				}
			}).observe({ type: 'layout-shift', buffered: true });
		});
		// A web font that swaps in late moves text by a pixel on any page; this counts the list's own shifts.
		await page.route('**/fonts/**', (route) => route.abort());
		await liveLogs(page);
		mock.on('getLogHistory', history([logLine(1), logLine(2, { level: 'warn' })]));
		mock.on('getLogCatalog', {
			status: 403,
			body: { error: { code: 'forbidden', message: 'no' } }
		});
		await go('/logs');
		await expect(page.getByText('message 2')).toBeVisible();
		await expect(page.getByRole('button', { name: 'Wrap lines' })).toHaveAttribute(
			'aria-pressed',
			'true'
		);

		// Visible labels, not placeholders alone.
		await expect(page.locator('label', { hasText: 'Minimum level' })).toBeVisible();
		await expect(page.locator('label', { hasText: 'Filter text' })).toBeVisible();

		// Nothing in the list moved while it loaded (the later presses are the user's own input).
		expect(await page.evaluate(() => window.__logShifts)).toEqual([]);

		// A component chip shows its state with an icon as well as colour.
		const chip = page.getByRole('button', { name: 'dht', exact: true });
		await chip.click();
		await expect(chip).toHaveAttribute('aria-pressed', 'true');
		await expect(chip.locator('svg')).toHaveCount(1);

		// Turning wrapping off by hand sticks.
		await page.getByRole('button', { name: 'Wrap lines' }).click();
		await expect(page.getByRole('button', { name: 'Wrap lines' })).toHaveAttribute(
			'aria-pressed',
			'false'
		);

		await expectNoA11yViolations(page);
	});

	test('a long message wraps instead of showing a few characters', async ({ page, go, mock }) => {
		await liveLogs(page);
		mock.on('getLogHistory', history([logLine(1, { msg: LONG })]));
		mock.on('getLogCatalog', {
			status: 403,
			body: { error: { code: 'forbidden', message: 'no' } }
		});
		await go('/logs');
		const msg = page.locator('.logs-msg').filter({ hasText: 'A long message' });
		await expect(msg).toBeVisible();
		const box = await msg.boundingBox();
		expect(box!.width).toBeGreaterThan(300);
		expect(box!.height).toBeGreaterThan(40);
		expect(await msg.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
	});

	test('an empty log says why and where to change it', async ({ page, go, mock }) => {
		await liveLogs(page);
		mock.on('getLogHistory', history([]));
		mock.on('getLogCatalog', {
			status: 403,
			body: { error: { code: 'forbidden', message: 'no' } }
		});
		await go('/logs');
		await expect(page.getByRole('heading', { name: 'No log lines yet' })).toBeVisible();
		await expect(page.getByRole('link', { name: 'Open logging settings' })).toHaveAttribute(
			'href',
			'/settings/logging'
		);
	});
});

test.describe('Discover at 390 px', () => {
	// A phone: the 44 px target rule applies under a coarse pointer.
	test.use({ hasTouch: true, isMobile: true });

	test('hides the sections that are not built and keeps Newest', async ({ page, go, mock }) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.on('getDiscover', discoverResponse());
		await go('/discover');
		await expect(page.getByTestId('discover-newest-total')).toBeVisible();
		await expect(page.getByTestId('discover-rising')).toHaveCount(0);
		await expect(page.getByTestId('discover-resolved')).toHaveCount(0);
		await expect(page.getByText(/Not available|coming soon/i)).toHaveCount(0);
		await expectNoA11yViolations(page);
	});

	test('the type tabs scroll sideways and keep 44 px targets', async ({ page, go, mock }) => {
		mock.on('getSettings', showAdultSettings(false));
		mock.on('getDiscover', response({ newest: newest({ hits: [hit(1)], total: 1 }) }));
		await go('/discover');
		const strip = page.locator('.dsc-tabs-box');
		await expect(strip).toBeVisible();
		expect(await strip.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
		await expect(page.getByText('The list of types scrolls sideways.')).toBeAttached();
		const tab = await page.getByTestId('discover-tab-movie').boundingBox();
		expect(tab!.height).toBeGreaterThanOrEqual(44);
	});
});
