// Keyboard-only convention (README): `*.keyboard.test.ts` files run in the `keyboard-only` project.
// `kbd` forbids page.mouse and fails the test if any pointer click reaches the page. Real flows
// (search → open → send; save alert; first-run wizard) are added by the slices that build them,
// in this file's style.
import { ROUTES } from './support/routes';
import { expect, test } from './support/test';

for (const route of ROUTES) {
	test(`keyboard: Tab order on ${route.name} never leaves the document`, async ({
		page,
		go,
		kbd
	}) => {
		await go(route.path);
		for (let i = 0; i < 5; i++) await kbd.press('Tab');
		const inside = await page.evaluate(() => document.activeElement !== null);
		expect(inside).toBe(true);
		for (let i = 0; i < 5; i++) await kbd.press('Shift+Tab');
		expect(await kbd.pointerEvents()).toEqual([]);
	});
}
