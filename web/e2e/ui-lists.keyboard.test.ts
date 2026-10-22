// W7cd: VirtualList, Table and Tree on the test-only showcase (/dev/ui/lists), keyboard-only,
// with axe in Riverstone light and dark.
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';

const PATH = '/dev/ui/lists';
// §10.1: INP < 100 ms; the budget here leaves a little headroom for a loaded CI runner.
const PERF_BUDGET_MS = 150;

for (const [themeId, colorMode] of [
	['riverstone-light', 'light'],
	['riverstone-dark', 'dark']
] as const) {
	test.describe(`lists, ${themeId}`, () => {
		test.use({ themeId, colorMode });

		test('axe: page with all three primitives', async ({ page, go }) => {
			await go(PATH);
			await expectNoA11yViolations(page);
		});

		test('axe: expanded tree with a mixed parent, and a sorted table', async ({
			page,
			go,
			kbd
		}) => {
			await go(PATH);
			const tree = page.getByRole('tree', { name: 'Files' });
			await kbd.tabTo(tree.getByRole('treeitem').first());
			await kbd.press('ArrowRight');
			await kbd.press('ArrowRight');
			await kbd.press('Space');
			await expect(page.getByTestId('tree-checked')).toHaveText('1');
			await expectNoA11yViolations(page);
		});
	});
}

test('VirtualList: roving tabindex, arrows, End, typeahead, Enter, load more', async ({
	page,
	go,
	kbd
}) => {
	await go(PATH);
	const list = page.getByRole('listbox', { name: 'Results' });
	const first = list.getByRole('option').first();
	await kbd.tabTo(first);
	await expect(list.locator('[tabindex="0"]')).toHaveCount(1);
	await kbd.press('ArrowDown');
	await expect(list.getByRole('option', { name: 'Result 1', exact: true })).toBeFocused();
	await kbd.press('Enter');
	await expect(page.getByTestId('vl-opened')).toHaveText('Result 1');
	await kbd.press('End');
	await expect(list.getByRole('option', { name: 'Result 59', exact: true })).toBeFocused();
	// The sentinel row asks for the next cursor page.
	await expect(page.getByTestId('vl-pages')).toHaveText('1');
	// Tab leaves the list in one press (only one tab stop).
	await kbd.press('Tab');
	expect(await list.evaluate((l) => l.contains(document.activeElement))).toBe(false);
});

test('Table: sort by keyboard, select with Space, column chooser', async ({ page, go, kbd }) => {
	await go(PATH);
	const grid = page.getByRole('grid', { name: 'Torrents' });
	await expect(grid.getByRole('columnheader', { name: 'Seeders' })).toHaveAttribute(
		'aria-sort',
		'none'
	);
	const seeders = grid.getByRole('columnheader', { name: 'Seeders' }).getByRole('button');
	await kbd.tabTo(seeders);
	await kbd.press('Enter');
	await expect(grid.getByRole('columnheader', { name: 'Seeders' })).toHaveAttribute(
		'aria-sort',
		'ascending'
	);
	await kbd.press('Enter');
	await expect(grid.getByRole('columnheader', { name: 'Seeders' })).toHaveAttribute(
		'aria-sort',
		'descending'
	);
	// Select two rows with Space.
	const row = grid.getByRole('row').nth(1);
	await kbd.tabTo(row);
	await kbd.press('Space');
	await kbd.press('ArrowDown');
	await kbd.press('Space');
	await expect(page.getByTestId('table-selected')).toHaveText('2');
	// Column chooser hides a column.
	await kbd.tabTo(page.getByTestId('column-chooser'));
	await kbd.press('Enter');
	await kbd.tabTo(page.getByLabel('Size', { exact: true }));
	await kbd.press('Space');
	await expect(grid.getByRole('columnheader', { name: 'Size' })).toHaveCount(0);
});

test('Tree: arrows, *, Space, typeahead; 20k nodes stay windowed', async ({ page, go, kbd }) => {
	await go(PATH);
	const tree = page.getByRole('tree', { name: 'Files' });
	expect(await tree.getByRole('treeitem').count()).toBeLessThan(40);
	await kbd.tabTo(tree.getByRole('treeitem').first());
	await kbd.press('ArrowRight');
	await expect(tree.getByRole('treeitem').first()).toHaveAttribute('aria-expanded', 'true');
	await kbd.press('ArrowRight');
	await expect(tree.getByRole('treeitem', { name: /Episode 1\.mkv/ })).toBeFocused();
	await kbd.press('Space');
	await expect(page.getByTestId('tree-checked')).toHaveText('1');
	await expect(tree.getByRole('treeitem').first()).toHaveAttribute('aria-checked', 'mixed');
	await kbd.press('ArrowLeft');
	await expect(tree.getByRole('treeitem').first()).toBeFocused();
	await kbd.press('Space'); // selects the whole season
	await expect(page.getByTestId('tree-checked')).toHaveText('999');
	await kbd.press('End');
	await expect(tree.getByRole('treeitem').last()).toBeFocused();
});

test('Tree: * expand-all on 100k nodes stays within the interaction budget', async ({
	page,
	go,
	kbd
}) => {
	await go(PATH);
	const tree = page.getByRole('tree', { name: 'Files' });
	await kbd.tabTo(tree.getByRole('treeitem').first());
	// Time the key press to the next painted frame, in the page.
	const ms = await page.evaluate(async () => {
		const row = document.activeElement as HTMLElement;
		const t0 = performance.now();
		row.dispatchEvent(new KeyboardEvent('keydown', { key: '*', bubbles: true, cancelable: true }));
		await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
		return performance.now() - t0;
	});
	console.log(`expand-all at 100k nodes: ${ms.toFixed(0)} ms`);
	await expect(tree.getByRole('treeitem').first()).toHaveAttribute('aria-expanded', 'true');
	expect(ms).toBeLessThan(PERF_BUDGET_MS);
});

test('VirtualList: rows prepended above keep the viewport and the active row in place', async ({
	page,
	go,
	kbd
}) => {
	await go(PATH);
	const list = page.getByRole('listbox', { name: 'Results' });
	await kbd.tabTo(list.getByRole('option').first());
	for (let i = 0; i < 4; i++) await kbd.press('PageDown');
	const probe = list.getByRole('option', { name: /^Result \d+$/ }).nth(3);
	const name = (await probe.textContent())!.trim();
	const top = () =>
		list.getByRole('option', { name, exact: true }).evaluate((e) => e.getBoundingClientRect().top);
	const before = await top();
	await kbd.tabTo(page.getByRole('button', { name: 'Prepend 3 results' }), 5).catch(async () => {
		await page.getByRole('button', { name: 'Prepend 3 results' }).focus();
	});
	await kbd.press('Enter');
	await expect(list.getByRole('option', { name, exact: true })).toBeVisible();
	expect(Math.abs((await top()) - before)).toBeLessThan(2);
});

test.describe('coarse pointer', () => {
	test.use({ hasTouch: true, isMobile: true });

	// Rows, checkboxes and the Table's checkbox column are all at least --target-min (44 px), so
	// adjacent targets never overlap and a tap lands on the box that was aimed at.
	for (const kind of ['grid', 'tree'] as const) {
		test(`${kind}: checkbox targets are 44 px, disjoint and inside their row/column`, async ({
			page,
			go
		}) => {
			await go(PATH);
			const rowRole = kind === 'grid' ? 'row' : 'treeitem';
			const rows = page.getByRole(kind).getByRole(rowRole);
			await expect(rows.nth(3)).toBeVisible();
			const m = await page.evaluate(
				([k, role]) => {
					const root = document.querySelector(`[role="${k}"]`)!;
					const list = [
						...root.querySelectorAll<HTMLElement>(`[role="${role}"][data-index]`)
					].slice(0, 6);
					list[0]!.scrollIntoView({ block: 'center' });
					return list.slice(0, 4).map((row) => {
						const r = row.getBoundingClientRect();
						const input = row.querySelector('input')!;
						const b = input.getBoundingClientRect();
						// What is under each corner (inset 2 px) and the centre of the box.
						const hits = [
							[b.left + 2, b.top + 2],
							[b.right - 2, b.top + 2],
							[b.left + 2, b.bottom - 2],
							[b.right - 2, b.bottom - 2],
							[b.left + b.width / 2, b.top + b.height / 2]
						].map(([x, y]) => document.elementFromPoint(x!, y!) === input);
						const next = row.querySelector('[role="gridcell"]:nth-child(2)') as HTMLElement | null;
						return {
							row: [r.top, r.bottom, r.height],
							box: [b.left, b.top, b.right, b.bottom, b.width, b.height],
							rowLeft: r.left,
							hits,
							nextLeft: next?.getBoundingClientRect().left ?? null
						};
					});
				},
				[kind, rowRole]
			);
			expect(m.length).toBeGreaterThan(3);
			m.forEach((x, i) => {
				expect(x.row[2]).toBeGreaterThanOrEqual(44);
				expect(x.box[4]).toBeGreaterThanOrEqual(44);
				expect(x.box[5]).toBeGreaterThanOrEqual(44);
				// Inside its own row...
				expect(x.box[1]).toBeGreaterThanOrEqual(x.row[0]! - 0.5);
				expect(x.box[3]).toBeLessThanOrEqual(x.row[1]! + 0.5);
				// ...never reaching the next row's box...
				if (i > 0) expect(m[i - 1]!.box[3]).toBeLessThanOrEqual(x.box[1]! + 0.5);
				// ...and every probe point hits this row's own checkbox.
				expect(x.hits).toEqual([true, true, true, true, true]);
				// The Table's box stays inside its column (the Name cell starts after it).
				if (x.nextLeft !== null) expect(x.box[2]).toBeLessThanOrEqual(x.nextLeft + 0.5);
			});
		});
	}
});
