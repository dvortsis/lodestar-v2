// S-detail keyboard-only: open the drawer, copy and open the magnet with c and o, and walk the
// file tree with the arrow keys. No pointer events are allowed (the kbd fixture fails the test).
import { expectNoA11yViolations } from './a11y';
import { detail, group, hit, searchBody, siblings } from './detail.fixtures';
import { expect, test } from './support/test';

const A = 'a'.repeat(40);

test.beforeEach(async ({ mock, context, page }) => {
	await context.grantPermissions(['clipboard-read', 'clipboard-write']);
	// Record the links `o` follows instead of navigating to a magnet: URL.
	await page.addInitScript(() => {
		const followed: string[] = [];
		(window as unknown as { followed: string[] }).followed = followed;
		HTMLAnchorElement.prototype.click = function () {
			followed.push(this.href);
		};
	});
	mock.on('getTorrent', detail({ hash: A, magnet: `magnet:?xt=urn:btih:${A}&dn=Show` }));
	mock.on('searchGet', searchBody([hit({ hash: A })]));
	mock.on('getTorrentSiblings', { ...siblings(A), other_versions: group() });
	mock.on('listTorrentFiles', { files: [], offset: 3, total: 3 });
});

test('open, copy the magnet, open the magnet, walk the tree', async ({ page, go, kbd }) => {
	await go('/dev/detail');
	await kbd.tabTo(page.getByTestId('open-a'));
	await kbd.press('Enter');
	await expect(page.getByRole('heading', { level: 2, name: 'Show' }).last()).toBeFocused();

	// c copies the open torrent's magnet
	await kbd.press('c');
	await expect(page.getByText('Magnet copied')).toBeVisible();
	expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
		`magnet:?xt=urn:btih:${A}&dn=Show`
	);

	// o follows it; the link is a plain magnet: href
	await expect(page.getByTestId('detail-open-magnet')).toHaveAttribute('href', /^magnet:\?xt=/);
	await kbd.press('o');
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { followed: string[] }).followed))
		.toEqual([`magnet:?xt=urn:btih:${A}&dn=Show`]);

	// the tree: Tab into it, then arrows
	const tree = page.getByRole('tree', { name: 'Files in this torrent' });
	await page.locator('[data-section="contents"]').scrollIntoViewIfNeeded();
	await expect(tree).toBeVisible();
	await kbd.tabTo(tree.getByRole('treeitem').first());
	const folder = tree.getByRole('treeitem', { name: /Show/ }).first();
	await expect(folder).toHaveAttribute('aria-expanded', 'false');
	await kbd.press('ArrowRight');
	await expect(folder).toHaveAttribute('aria-expanded', 'true');
	await kbd.press('ArrowDown'); // readme.txt sorts before S01E01 (case-insensitive)
	await expect(tree.getByRole('treeitem', { name: /readme\.txt/ })).toBeFocused();
	await kbd.press('ArrowDown');
	await expect(tree.getByRole('treeitem', { name: /S01E01\.mkv/ })).toBeFocused();
	await kbd.press('ArrowLeft'); // to the parent
	await expect(folder).toBeFocused();
	await kbd.press('ArrowLeft'); // collapse
	await expect(folder).toHaveAttribute('aria-expanded', 'false');
});

test('Esc closes the drawer from the keyboard and returns focus', async ({ page, go, kbd }) => {
	await go('/dev/detail');
	await kbd.tabTo(page.getByTestId('open-a'));
	await kbd.press('Enter');
	await expect(page.getByTestId('drawer')).toBeVisible();
	await kbd.press('Escape');
	await expect(page.getByTestId('drawer')).toHaveCount(0);
	await expect(page.getByTestId('open-a')).toBeFocused();
});

test('the drawer is axe-clean once loaded', async ({ page, go, kbd }) => {
	await go('/dev/detail');
	await kbd.tabTo(page.getByTestId('open-a'));
	await kbd.press('Enter');
	await expect(page.getByTestId('detail-size')).toBeVisible();
	await page.evaluate(() =>
		Promise.all(
			document
				.getAnimations()
				.filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
				.map((a) => a.finished)
		)
	);
	await expectNoA11yViolations(page);
});
