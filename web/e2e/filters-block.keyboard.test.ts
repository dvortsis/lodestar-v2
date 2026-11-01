// S-filters-2c, keyboard only: "Block this…" from a result's More menu and from the detail drawer.
// The dialog previews first, applies only an added rule, and Esc sends nothing; axe on the dialog.
// No pointer events are allowed (the kbd fixture fails the test).
import type { Page } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { body, expect, hashOf, hits, rowOf, server } from './search-results.fixtures';
import { test } from './support/test';

type Assessment = components['schemas']['Assessment'];
const strengthening: Assessment = {
	weakening: false,
	confirm: false,
	warning: null,
	preview: {
		quarantine: 0,
		tombstones: 0,
		stored: 1,
		removed_from_floor: 0,
		caught_30d: 0,
		core_caught_30d: 0,
		per_day: 0,
		estimate: false
	},
	neutralize: null
};

function flows(mock: OpenApiMock) {
	mock.on('searchGet', body([], { total: 1 }));
	mock.on('startFilterPreviewJob', { status: 202, body: { job: 1 } });
	mock.on('getFilterPreviewJob', {
		state: 'done',
		progress: { phase: 'done', done: 1, total: 1 },
		assessment: strengthening
	});
	mock.on('cancelFilterPreviewJob', { ok: true });
	mock.on('applyFilterChange', { applied: true, assessment: strengthening, version: 4 });
}

const blockDialog = (page: Page) => page.getByRole('dialog', { name: 'Block this torrent' });

/** Open, opaque and done animating (a skeleton's endless pulse aside), so axe reads settled colours and keys land inside. */
async function settled(page: Page) {
	await expect(blockDialog(page)).toBeVisible();
	await expect
		.poll(
			() =>
				blockDialog(page).evaluate(
					(el) =>
						el.getAttribute('data-state') === 'open' &&
						Number(getComputedStyle(el).opacity) === 1 &&
						document
							.getAnimations()
							.every(
								(a) =>
									a.playState !== 'running' || a.effect?.getComputedTiming().iterations === Infinity
							)
				),
			{ timeout: 15_000 }
		)
		.toBe(true);
}

test.beforeEach(({ mock }) => {
	server(mock, () => body(hits(3)));
	flows(mock);
});

test('from a result: Shift+F10, Block this…, preview, Apply adds one rule', async ({
	page,
	go,
	mock,
	kbd
}) => {
	await go('/search?q=lantern');
	await expect(rowOf(page, 1)).toBeVisible();
	await kbd.tabTo(rowOf(page, 1));
	await kbd.press('Shift+F10');
	const item = page.getByRole('menuitem', { name: 'Block this…' });
	await expect(item).toBeVisible();
	await kbd.press('End');
	await expect(item).toBeFocused();
	await kbd.press('Enter');
	await settled(page);
	await expect(page.getByTestId('block-query')).toHaveText(`hash:${hashOf(1)}`);
	await expectNoA11yViolations(page, { include: ['[role="dialog"]'] });
	await kbd.tabTo(page.getByTestId('block-preview'));
	await kbd.press('Enter');
	const apply = page.getByTestId('change-apply');
	await expect(apply).toBeVisible();
	await kbd.tabTo(apply);
	await kbd.press('Enter');
	await expect(page.getByText('What changed')).toBeVisible();
	const sent = mock.calls('applyFilterChange').map((r) => r.postDataJSON().change);
	expect(sent).toEqual([{ op: 'add_rule', query: `hash:${hashOf(1)}`, action: 'hide' }]);
	await kbd.press('Escape');
	await expect(blockDialog(page)).toHaveCount(0);
	mock.assertNoFailures();
});

test('from the drawer: Block this…, then Esc sends nothing and focus comes back', async ({
	page,
	go,
	mock,
	kbd
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await go('/search?q=lantern');
	await expect(rowOf(page, 2)).toBeVisible();
	await kbd.tabTo(rowOf(page, 1));
	await kbd.press('j');
	await kbd.press('Enter');
	await expect(page.getByTestId('drawer')).toBeVisible();
	const invoker = page.getByTestId('detail-block');
	await expect(invoker).toBeEnabled();
	await kbd.tabTo(invoker);
	await kbd.press('Enter');
	await settled(page);
	await expect(page.getByTestId('block-query')).toHaveText(`hash:${hashOf(2)}`);
	await expectNoA11yViolations(page, { include: ['[role="dialog"]'] });
	await kbd.press('Escape');
	await expect(blockDialog(page)).toHaveCount(0);
	await expect(invoker).toBeFocused();
	expect(mock.calls('startFilterPreviewJob')).toHaveLength(0);
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
	await expect(page.getByTestId('drawer')).toBeVisible();
});
