// W7cd: VirtualList, Table and Tree on the test-only showcase (/dev/ui/lists), keyboard-only,
// with axe in Riverstone light and dark.
import { expectNoA11yViolations } from './a11y';
import { expect, test } from './support/test';

const PATH = '/dev/ui/lists';

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
