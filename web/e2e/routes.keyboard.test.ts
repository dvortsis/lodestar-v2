// Keyboard-only convention (README): `*.keyboard.test.ts` files run in the `keyboard-only` project.
// `kbd` forbids page.mouse and fails the test if any pointer event reaches the page. Real flows
// (search → open → send; save alert; first-run wizard) are added by the slices that build them,
// in this file's style.
import { ROUTES } from './support/routes';
import { expect, test } from './support/test';

// The first Tab on every route lands on the skip link (the shell, W9), which must show a focus ring.

for (const route of ROUTES) {
	test(`keyboard: first Tab on ${route.name} lands on a visibly focused control`, async ({
		page,
		go,
		kbd
	}) => {
		await go(route.path);
		// The app mounts after load; the skip link is the first thing it renders.
		await page.getByRole('link', { name: 'Skip to main content' }).waitFor();
		// A page may move focus into its form once it has loaded (the setup and sign-in screens do):
		// start from the top of the document, as a person arriving fresh does.
		await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
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
