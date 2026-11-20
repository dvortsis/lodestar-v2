// X-results-drawer-adaptive (owner §2.66 items 1 to 3; architecture §10.15; UI review I1, I3, L5):
// from 1366 px the detail drawer sits beside the results instead of over them, result rows adapt
// to their own width, a fact that doesn't fit drops whole instead of clipping mid-word, also under
// WCAG 1.4.12 text spacing, and Discover's Newest leads with the discovered time. Name ends in
// `routes.test.ts` so the `core` project runs it.
import type { Locator, Page } from '@playwright/test';
import { hit, newest, response, showAdultSettings } from '../src/lib/features/discover/testing';
import { body, expect, grid, hashOf, hits, row, rowOf, server } from './search-results.fixtures';
import { expectPageSnapshot, freezeClock } from './snapshot';
import { test } from './support/test';

// The text-spacing check injects a style sheet; the preview's CSP would refuse it.
test.use({ bypassCSP: true });

const host = (page: Page) => page.locator('.ls-detail-host');
const drawer = (page: Page) => page.getByTestId('drawer');
const panel = (page: Page) => page.locator('.rp');

async function openRow(page: Page, n: number) {
	await expect(row(page, n)).toBeVisible();
	await rowOf(page, n).focus();
	await page.keyboard.press('Enter');
	await expect(drawer(page)).toBeVisible();
}

const box = async (l: Locator) => {
	const b = await l.boundingBox();
	expect(b).not.toBeNull();
	return b!;
};

/** Every fact on the visible rows is either wholly inside its facts line or wrapped out of it:
 * none is cut at the line's end. Returns how many facts were checked. */
async function factsWhole(page: Page): Promise<number> {
	const cut = await page.evaluate(() => {
		const bad: string[] = [];
		let seen = 0;
		for (const line of document.querySelectorAll<HTMLElement>('.rr-facts')) {
			const l = line.getBoundingClientRect();
			if (l.width === 0) continue;
			for (const fact of line.children) {
				const f = fact.getBoundingClientRect();
				if (f.width === 0) continue; // display: none in this tier
				seen++;
				const wrapped = f.top >= l.bottom - 1;
				const inside = f.right <= l.right + 1 && f.left >= l.left - 1;
				if (!wrapped && !inside) bad.push(`${fact.className}: ${fact.textContent?.trim()}`);
			}
		}
		return { bad, seen };
	});
	expect(cut.bad, 'facts cut at the end of their line').toEqual([]);
	return cut.seen;
}

/** The fact is on its line's visible part: not wrapped out, not cut at the end. `toBeVisible`
 * can't tell, since it ignores clipping by `overflow: hidden`. */
async function onLine(l: Locator) {
	const shown = await l.evaluate((el) => {
		const fact = el.closest('.rr-facts > *') ?? el;
		const line = fact.closest('.rr-facts')!.getBoundingClientRect();
		const f = fact.getBoundingClientRect();
		return f.width > 0 && f.top < line.bottom - 1 && f.right <= line.right + 1;
	});
	expect(shown).toBe(true);
}

/** The control's centre is its own: nothing (the drawer) covers it. */
async function uncovered(l: Locator) {
	const hitSelf = await l.evaluate((el) => {
		const r = el.getBoundingClientRect();
		const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
		return top !== null && (top === el || el.contains(top));
	});
	expect(hitSelf).toBe(true);
}

test.describe('the drawer beside the results', () => {
	for (const width of [1366, 1440]) {
		test(`at ${width} px it sits beside the results at its minimum width`, async ({
			page,
			go,
			mock
		}) => {
			await page.setViewportSize({ width, height: 900 });
			server(mock, () => body(hits(6)));
			await go('/search?q=lantern');
			await openRow(page, 2);
			await expect(host(page)).toHaveAttribute('data-mode', 'inline');
			const d = await box(drawer(page));
			const r = await box(panel(page));
			// Beside, not over: the results end where the drawer begins.
			expect(r.x + r.width).toBeLessThanOrEqual(d.x + 1);
			// The results keep at least the 480 px floor (§2.66).
			expect(r.width).toBeGreaterThanOrEqual(480);
			// The drawer opens at --drawer-min-width until the window fits more.
			expect(d.width).toBeGreaterThanOrEqual(480);
			expect(d.width).toBeLessThanOrEqual(560);
			// The toolbar and every row stay usable.
			await uncovered(page.getByTestId('results-count'));
			// The toolbar's last control, the first the old overlay covered (I1).
			await uncovered(page.getByTestId('results-toolbar').getByRole('button').last());
			await uncovered(rowOf(page, 1));
			await factsWhole(page);
		});
	}

	test('at 1366 px the drawer is 480 px and the results about 542 px', async ({
		page,
		go,
		mock
	}) => {
		await page.setViewportSize({ width: 1366, height: 900 });
		server(mock, () => body(hits(3)));
		await go('/search?q=lantern');
		await openRow(page, 1);
		await expect(host(page)).toHaveAttribute('data-mode', 'inline');
		expect((await box(drawer(page))).width).toBe(480);
		// About 542 px with overlay scrollbars; a classic scrollbar takes its width from it.
		expect((await box(panel(page))).width).toBeGreaterThanOrEqual(480);
	});

	test('a stored width is used once the window fits it', async ({ page, go, mock }) => {
		await page.addInitScript(() => localStorage.setItem('lodestar.drawer.width', '640'));
		server(mock, () => body(hits(3)));
		// Too narrow for 640 beside 560 px of results: the drawer gives way first.
		await page.setViewportSize({ width: 1440, height: 900 });
		await go('/search?q=lantern');
		await openRow(page, 1);
		await expect(host(page)).toHaveAttribute('data-mode', 'inline');
		const narrow = (await box(drawer(page))).width;
		expect(narrow).toBeGreaterThanOrEqual(480);
		expect(narrow).toBeLessThan(640);
		expect((await box(panel(page))).width).toBeGreaterThanOrEqual(480);
		// Wide enough: the stored width.
		await page.setViewportSize({ width: 1920, height: 900 });
		await expect.poll(async () => (await box(drawer(page))).width).toBe(640);
		// Resizing never stored the narrowed width.
		expect(await page.evaluate(() => localStorage.getItem('lodestar.drawer.width'))).toBe('640');
	});

	test('widening by keyboard at the cap stores nothing', async ({ page, go, mock }) => {
		await page.addInitScript(() => localStorage.setItem('lodestar.drawer.width', '640'));
		server(mock, () => body(hits(3)));
		await page.setViewportSize({ width: 1440, height: 900 });
		await go('/search?q=lantern');
		await openRow(page, 1);
		const handle = drawer(page).getByRole('separator');
		const capped = Number(await handle.getAttribute('aria-valuenow'));
		expect(capped).toBeLessThan(640);
		await handle.focus();
		await page.keyboard.press('ArrowLeft');
		await page.keyboard.press('Home');
		await expect(handle).toHaveAttribute('aria-valuenow', String(capped));
		expect(await page.evaluate(() => localStorage.getItem('lodestar.drawer.width'))).toBe('640');
		// Narrowing is a choice, and is stored.
		await page.keyboard.press('ArrowRight');
		await expect
			.poll(() => page.evaluate(() => localStorage.getItem('lodestar.drawer.width')))
			.toBe(String(capped - 16));
	});

	test('below 1366 px it still overlays, keeping the list visible', async ({ page, go, mock }) => {
		await page.setViewportSize({ width: 1365, height: 900 });
		server(mock, () => body(hits(3)));
		await go('/search?q=lantern');
		await openRow(page, 1);
		await expect(host(page)).toHaveAttribute('data-mode', 'overlay');
		await expect(grid(page)).toBeVisible();
	});

	test('the 1440 px cell with the drawer open', async ({ page, go, mock }) => {
		await freezeClock(page);
		await page.setViewportSize({ width: 1440, height: 900 });
		server(mock, () => body(hits(6)));
		await go('/search?q=lantern');
		await openRow(page, 2);
		await expect(host(page)).toHaveAttribute('data-mode', 'inline');
		await expectPageSnapshot(page, 'search-drawer-1440');
	});
});

test.describe('rows adapt to their own width', () => {
	for (const width of [390, 768, 1024, 1366, 1920]) {
		test(`no fact is cut mid-word at ${width} px`, async ({ page, go, mock }) => {
			await page.setViewportSize({ width, height: 900 });
			server(mock, () => body(hits(6)));
			await go('/search?q=lantern');
			await expect(row(page, 1)).toBeVisible();
			expect(await factsWhole(page)).toBeGreaterThan(0);
			// Size, availability and the discovered time survive every width (I3).
			const id = `result-${hashOf(1)}`;
			for (const fact of ['size', 'swarm', 'discovered'])
				await onLine(page.getByTestId(`${id}-${fact}`));
		});
	}

	test.describe('compact', () => {
		test.use({ density: 'compact' });
		test('compact rows cut no fact either', async ({ page, go, mock }) => {
			await page.setViewportSize({ width: 768, height: 900 });
			server(mock, () => body(hits(6)));
			await go('/search?q=lantern');
			await expect(row(page, 1)).toHaveAttribute('data-density', 'compact');
			expect(await factsWhole(page)).toBeGreaterThan(0);
		});
	});

	test('under WCAG 1.4.12 text spacing, facts still drop whole', async ({ page, go, mock }) => {
		await page.setViewportSize({ width: 1024, height: 900 });
		server(mock, () => body(hits(6)));
		await go('/search?q=lantern');
		await expect(row(page, 1)).toBeVisible();
		await page.addStyleTag({
			content:
				'* { line-height: 1.5 !important; letter-spacing: 0.12em !important; word-spacing: 0.16em !important; } p { margin-bottom: 2em !important; }'
		});
		expect(await factsWhole(page)).toBeGreaterThan(0);
	});
});

test('Discover Newest leads with the discovered time', async ({ page, go, mock }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	mock.on('getSettings', showAdultSettings(false));
	mock.on('getDiscover', () =>
		response({ newest: newest({ hits: [hit(1), hit(2)], total: 2, type: null }) })
	);
	await go('/discover');
	const section = page.locator('[aria-labelledby="discover-newest-h"]');
	const facts = section.locator('.rr-facts').first();
	await expect(facts).toBeVisible();
	// Ordered by `order`: the leftmost fact on the line's visible part is the discovered time.
	const leftmost = await facts.evaluate((line) => {
		const bottom = line.getBoundingClientRect().bottom;
		const shown = [...line.children]
			.map((e) => ({ e, r: e.getBoundingClientRect() }))
			.filter(({ r }) => r.width > 0 && r.top < bottom - 1);
		shown.sort((a, b) => a.r.left - b.r.left);
		return shown[0]?.e.classList.contains('rr-discovered') ?? false;
	});
	expect(leftmost).toBe(true);
});
