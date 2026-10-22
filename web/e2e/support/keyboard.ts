// Keyboard-only convention (architecture §10.17): a flow that must work without a pointer is
// written against `KeyboardOnly`. It makes `page.mouse` throw and records any pointer-generated
// click in the page; the fixture fails the test at teardown if one happened.
import type { Locator, Page } from '@playwright/test';

declare global {
	interface Window {
		__pointerUsed?: string[];
	}
}

const MOUSE_METHODS = ['click', 'dblclick', 'down', 'up', 'move', 'wheel'] as const;

export class KeyboardOnly {
	constructor(private page: Page) {}

	async install() {
		for (const m of MOUSE_METHODS) {
			(this.page.mouse as unknown as Record<string, unknown>)[m] = () => {
				throw new Error(`keyboard-only test used page.mouse.${m}()`);
			};
		}
		// A trusted pointerdown, a pointermove that actually moved, or a click with detail > 0 came
		// from a pointer. Not pointerover/enter: the browser fires those itself when layout changes
		// under a stationary cursor. Keyboard activation (Enter/Space) clicks with detail 0.
		await this.page.addInitScript(() => {
			window.__pointerUsed = [];
			const note = (e: Event) =>
				window.__pointerUsed!.push(`${e.type}:${(e.target as Element)?.tagName}`);
			addEventListener('pointerdown', (e) => e.isTrusted && note(e), true);
			addEventListener(
				'pointermove',
				(e) => e.isTrusted && (e.movementX !== 0 || e.movementY !== 0) && note(e),
				true
			);
			addEventListener('click', (e) => e.isTrusted && e.detail > 0 && note(e), true);
		});
	}

	/** Tab forward until `target` has focus (at most `max` presses), returning the presses used. */
	async tabTo(target: Locator, max = 50): Promise<number> {
		for (let i = 1; i <= max; i++) {
			await this.page.keyboard.press('Tab');
			if (await target.evaluate((el) => el === document.activeElement).catch(() => false)) return i;
		}
		throw new Error(`keyboard-only: ${target} not reached within ${max} Tab presses`);
	}

	async press(key: string) {
		await this.page.keyboard.press(key);
	}

	async pointerEvents(): Promise<string[]> {
		return this.page.evaluate(() => window.__pointerUsed ?? []);
	}
}
