// UI check (review 03 "Not done": overlay timings and radius): dialogs, sheets, the drawer,
// popovers, menus and toasts have the radius and motion the design tokens give them. The review
// measured resting transitions only; this reads the computed animation and corner radii of each
// overlay while it is open and while it is leaving, on the showcase pages under /dev/ui.
//   dialog and sheet   radius 16 px (design-tokens §6); sheet slides in 300 ms, out 200 ms (§9);
//                      the dialog fades in 150 ms and out 200 ms (src/lib/ui/overlays/overlays.css)
//   drawer             start corners 16 px; slides in 300 ms, out 200 ms (§9)
//   popover and menu   radius 12 px; fade in 150 ms, out 100 ms (§9)
//   toast              radius 12 px; fades in over --duration-banner, 200 ms (§9)
// The exit is read from an `animationstart` listener (or the page's own animation call for the
// drawer), because the panel is removed when the exit ends. Waits are conditions with a 10 s
// deadline; no fixed sleeps. Named `.routes.test.ts` so the `core` project picks it up.
import type { Locator, Page } from '@playwright/test';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });
const OVERLAYS = '/dev/ui/overlays';

interface Look {
	name: string;
	duration: string;
	radius: string[];
}

/** The computed animation and the four corner radii (tl, tr, br, bl) of an overlay panel. */
const look = (panel: Locator): Promise<Look> =>
	panel.evaluate((el) => {
		const cs = getComputedStyle(el);
		return {
			name: cs.animationName,
			duration: cs.animationDuration,
			radius: [
				cs.borderTopLeftRadius,
				cs.borderTopRightRadius,
				cs.borderBottomRightRadius,
				cs.borderBottomLeftRadius
			]
		};
	});

const ALL_16 = ['16px', '16px', '16px', '16px'];
const ALL_12 = ['12px', '12px', '12px', '12px'];

/**
 * Records the exit animation the moment it starts, in the page: the panel is removed when the exit
 * ends, so reading it afterwards from the test can find it gone. Only an animation whose name ends
 * in `-out` counts, so a late entrance animation cannot be taken for the exit.
 */
async function recordExit(panel: Locator) {
	await panel.evaluate((el) => {
		el.addEventListener('animationstart', (event) => {
			const { animationName } = event as AnimationEvent;
			if (!animationName.endsWith('-out')) return;
			(window as unknown as { __exit?: unknown }).__exit = {
				name: animationName,
				duration: getComputedStyle(el).animationDuration
			};
		});
	});
}
const exit = (page: Page) =>
	page.evaluate(() => (window as unknown as { __exit?: Look }).__exit ?? null);

/**
 * The drawer moves with a Svelte transition, which runs as a Web Animation, not a CSS animation:
 * record every `animate()` call made on a test-id'd element, with its duration.
 */
async function recordWebAnimations(page: Page) {
	await page.addInitScript(() => {
		const calls: { id: string | null; duration: unknown }[] = [];
		(window as unknown as { __animate: typeof calls }).__animate = calls;
		const animate = Element.prototype.animate;
		Element.prototype.animate = function (this: Element, keyframes, options) {
			const duration = typeof options === 'number' ? options : options?.duration;
			calls.push({ id: this.getAttribute('data-testid'), duration });
			return animate.call(this, keyframes, options);
		};
	});
}
const durations = (page: Page, id: string) =>
	page.evaluate(
		(want) =>
			(window as unknown as { __animate: { id: string | null; duration: number }[] }).__animate
				// A 0 ms call is Svelte's own bookkeeping, not a movement.
				.filter((c) => c.id === want && c.duration > 0)
				.map((c) => c.duration),
		id
	);

test.describe('overlay timings and radius', () => {
	test('a dialog is 16 px all round, fades in over 150 ms and out over 200 ms', async ({
		page,
		go
	}) => {
		await go(OVERLAYS);
		await page.getByRole('button', { name: 'Open dialog' }).click();
		const dialog = page.getByRole('dialog', { name: 'Remove torrent' });
		await expect(dialog).toHaveAttribute('data-state', 'open');
		const open = await look(dialog);
		expect(open.name).toBe('ls-fade-in');
		expect(open.duration).toBe('0.15s');
		expect(open.radius).toEqual(ALL_16);
		await recordExit(dialog);
		await page.keyboard.press('Escape');
		await expect.poll(() => exit(page)).toEqual({ name: 'ls-fade-out', duration: '0.2s' });
	});

	test('a bottom sheet is 16 px on its top corners, slides in over 300 ms and out over 200 ms', async ({
		page,
		go
	}) => {
		await go(OVERLAYS);
		await page.getByRole('button', { name: 'Open sheet' }).click();
		const sheet = page.getByRole('dialog', { name: 'Filters' });
		await expect(sheet).toHaveAttribute('data-state', 'open');
		const open = await look(sheet);
		expect(open.name).toBe('ls-slide-bottom-in');
		expect(open.duration).toBe('0.3s');
		expect(open.radius).toEqual(['16px', '16px', '0px', '0px']);
		await recordExit(sheet);
		await page.keyboard.press('Escape');
		await expect.poll(() => exit(page)).toEqual({ name: 'ls-slide-bottom-out', duration: '0.2s' });
	});

	test('the drawer is 16 px on its start corners and slides in over 300 ms and out over 200 ms', async ({
		page,
		go
	}) => {
		await recordWebAnimations(page);
		await go(OVERLAYS);
		await page.getByRole('button', { name: 'Ubuntu 24.04 desktop' }).click();
		const drawer = page.getByTestId('drawer');
		await expect(drawer).toBeVisible();
		await expect.poll(() => durations(page, 'drawer')).toEqual([300]);
		// rounded-s-sheet: the leading (start) side, the left in a left-to-right page.
		expect((await look(drawer)).radius).toEqual(['16px', '0px', '0px', '16px']);
		await page.getByRole('button', { name: 'Close' }).click();
		await expect.poll(() => durations(page, 'drawer')).toEqual([300, 200]);
	});

	test('a popover is 12 px, fades in over 150 ms and out over 100 ms', async ({ page, go }) => {
		await go(OVERLAYS);
		await page.getByRole('button', { name: 'Columns' }).click();
		const popover = page.getByRole('dialog', { name: 'Columns' });
		await expect(popover).toHaveAttribute('data-state', 'open');
		const open = await look(popover);
		expect(open.name).toBe('ls-fade-in');
		expect(open.duration).toBe('0.15s');
		expect(open.radius).toEqual(ALL_12);
		await recordExit(popover);
		await page.keyboard.press('Escape');
		await expect.poll(() => exit(page)).toEqual({ name: 'ls-fade-out', duration: '0.1s' });
	});

	test('a menu is 12 px, fades in over 150 ms and out over 100 ms', async ({ page, go }) => {
		await go(OVERLAYS);
		await page.getByRole('button', { name: 'Actions' }).click();
		const menu = page.getByRole('menu', { name: 'Torrent actions' });
		await expect(menu).toHaveAttribute('data-state', 'open');
		const open = await look(menu);
		expect(open.name).toBe('ls-fade-in');
		expect(open.duration).toBe('0.15s');
		expect(open.radius).toEqual(ALL_12);
		await recordExit(menu);
		await page.keyboard.press('Escape');
		await expect.poll(() => exit(page)).toEqual({ name: 'ls-fade-out', duration: '0.1s' });
	});

	test('a toast is 12 px and fades in over 200 ms', async ({ page, go }) => {
		await go('/dev/ui/feedback');
		await page.getByRole('button', { name: 'Toast success' }).click();
		const toast = page.getByTestId('toast');
		await expect(toast).toBeVisible();
		const shown = await look(toast);
		// Svelte scopes the keyframes name (`svelte-<hash>-toast-in`).
		expect(shown.name).toMatch(/toast-in$/);
		expect(shown.duration).toBe('0.2s');
		expect(shown.radius).toEqual(ALL_12);
	});
});
