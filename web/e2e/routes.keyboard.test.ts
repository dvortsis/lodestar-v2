// Keyboard-only convention (README): `*.keyboard.test.ts` files run in the `keyboard-only` project.
// `kbd` forbids page.mouse and fails the test if any pointer event reaches the page. Real flows
// (search → open → send; save alert; first-run wizard) are added by the slices that build them,
// in this file's style.
import { ROUTES } from './support/routes';
import { expect, test } from './support/test';

// The routes are `<h1>` stubs with nothing focusable yet, so there is no focus behaviour to assert.
// Drop the fixme once the shell lands (skip link / top bar): the body below is the check.
test.fixme(true, 'no interactive UI yet (shell slice)');

for (const route of ROUTES) {
	test(`keyboard: first Tab on ${route.name} lands on a visibly focused control`, async ({
		page,
		go,
		kbd
	}) => {
		await go(route.path);
		await kbd.press('Tab');
		const focus = await page.evaluate(() => {
			const el = document.activeElement;
			if (!el || el === document.body) return null;
			const s = getComputedStyle(el);
			return {
				focusVisible: el.matches(':focus-visible'),
				ring:
					(s.outlineStyle !== 'none' && parseFloat(s.outlineWidth) > 0) || s.boxShadow !== 'none'
			};
		});
		expect(focus, 'a control has focus').not.toBeNull();
		expect(focus).toEqual({ focusVisible: true, ring: true });
		expect(await kbd.pointerEvents()).toEqual([]);
	});
}
