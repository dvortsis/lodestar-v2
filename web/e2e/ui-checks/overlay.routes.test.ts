// UI check (review 03 "Not done": overlay timings and radius): the design tokens say dialogs and
// sheets have a 16 px radius, fade in over --duration-fast, slide in over 300 ms and leave over
// 200 ms. The review measured resting transitions only; this reads the computed animation and
// radius of each overlay while it is open and while it is leaving, on the overlay showcase page.
// The open/closed `data-state` is the condition waited on (10 s deadline); no fixed sleeps.
// Named `.routes.test.ts` so the `core` project picks it up.
import type { Locator } from '@playwright/test';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });
const PAGE = '/dev/ui/overlays';

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

test.describe('overlay timings and radius', () => {
	test('a dialog is 16 px all round, fades in over 150 ms and out over 200 ms', async ({
		page,
		go
	}) => {
		await go(PAGE);
		await page.getByRole('button', { name: 'Open dialog' }).click();
		const dialog = page.getByRole('dialog', { name: 'Remove torrent' });
		await expect(dialog).toHaveAttribute('data-state', 'open');
		const open = await look(dialog);
		expect(open.name).toBe('ls-fade-in');
		expect(open.duration).toBe('0.15s');
		expect(open.radius).toEqual(['16px', '16px', '16px', '16px']);
		await page.keyboard.press('Escape');
		await expect(dialog).toHaveAttribute('data-state', 'closed');
		const closing = await look(dialog);
		expect(closing.name).toBe('ls-fade-out');
		expect(closing.duration).toBe('0.2s');
	});

	test('a bottom sheet is 16 px on its top corners, slides in over 300 ms and out over 200 ms', async ({
		page,
		go
	}) => {
		await go(PAGE);
		await page.getByRole('button', { name: 'Open sheet' }).click();
		const sheet = page.getByRole('dialog', { name: 'Filters' });
		await expect(sheet).toHaveAttribute('data-state', 'open');
		const open = await look(sheet);
		expect(open.name).toBe('ls-slide-bottom-in');
		expect(open.duration).toBe('0.3s');
		expect(open.radius).toEqual(['16px', '16px', '0px', '0px']);
		await page.keyboard.press('Escape');
		await expect(sheet).toHaveAttribute('data-state', 'closed');
		const closing = await look(sheet);
		expect(closing.name).toBe('ls-slide-bottom-out');
		expect(closing.duration).toBe('0.2s');
	});
});
