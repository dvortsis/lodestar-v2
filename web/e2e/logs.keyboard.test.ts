// §10.10.12: the live log at /logs against the mock, keyboard only, with axe.
import { expectNoA11yViolations } from './a11y';
import { liveLogs, logLine } from './logs.stream';
import { expect, test } from './support/test';

test.use({ viewport: { width: 1366, height: 800 } });

const history = (lines: ReturnType<typeof logLine>[]) => ({
	lines: lines.toReversed(),
	more: false,
	next_cursor: null,
	unreadable: 0
});

test('shows history, follows streamed lines, and escapes markup in them', async ({
	page,
	go,
	mock
}) => {
	const stream = await liveLogs(page);
	mock.on('getLogHistory', history([logLine(1), logLine(2, { level: 'warn' })]));
	mock.on('getLogCatalog', { status: 403, body: { error: { code: 'forbidden', message: 'no' } } });
	await go('/logs');
	await expect(page.getByText('message 2')).toBeVisible();
	await stream.opened();
	await stream.send([logLine(3, { msg: '<script>window.__pwned=1</script>' }), logLine(4)]);
	await expect(page.getByText('<script>window.__pwned=1</script>')).toBeVisible({
		timeout: 10_000
	});
	await expect(page.getByText('message 4')).toBeVisible();
	expect(
		await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)
	).toBeUndefined();
	await expectNoA11yViolations(page);
});

test('keys: / filters, Space pauses, Enter expands a row; the filter goes to the server', async ({
	page,
	go,
	kbd,
	mock
}) => {
	const stream = await liveLogs(page);
	mock.on(
		'getLogHistory',
		history([logLine(1, { fields: [{ kind: 'label', name: 'peer', value: 'abc' }] })])
	);
	mock.on('getLogCatalog', { status: 403, body: { error: { code: 'forbidden', message: 'no' } } });
	await go('/logs');
	await expect(page.getByText('message 1')).toBeVisible();
	await stream.opened();

	await kbd.press('/');
	await expect(page.getByLabel('Filter text')).toBeFocused();
	await page.keyboard.type('needle');
	await expect
		.poll(async () => (await stream.urls()).some((u) => u.includes('text=needle')), {
			timeout: 10_000
		})
		.toBe(true);
	expect(mock.calls('getLogHistory').some((r) => r.url().includes('text=needle'))).toBe(true);

	await expect(page.getByText('message 1')).toBeVisible();
	// Pause by key from the body, then a line that arrives stays out of view until Resume.
	await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
	await kbd.press('Space');
	await expect(page.getByRole('button', { name: 'Resume' })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	await stream.send([logLine(9, { msg: 'held line' })]);
	await expect(page.getByText('Paused · 1 new lines waiting')).toBeVisible({ timeout: 10_000 });
	await expect(page.getByText('held line')).toHaveCount(0);
	await kbd.press('Space');
	await expect(page.getByText('held line')).toBeVisible({ timeout: 10_000 });
	expect(await kbd.pointerEvents()).toEqual([]);
});

test('Enter expands a row to show its fields', async ({ page, go, kbd, mock }) => {
	await liveLogs(page);
	mock.on(
		'getLogHistory',
		history([logLine(1, { fields: [{ kind: 'label', name: 'peer', value: 'abc<b>x</b>' }] })])
	);
	mock.on('getLogCatalog', { status: 403, body: { error: { code: 'forbidden', message: 'no' } } });
	await go('/logs');
	await expect(page.getByText('message 1')).toBeVisible();
	await kbd.tabTo(page.locator('[role=listbox][aria-label="Log lines"] [role=option]').first());
	await kbd.press('Enter');
	await expect(page.getByText('abc<b>x</b>')).toBeVisible();
	await kbd.press('Enter');
	await expect(page.getByText('abc<b>x</b>')).toHaveCount(0);
});

test('scrolling up stops following, shows Jump to latest, and Jump returns to the bottom', async ({
	page,
	go,
	mock
}) => {
	const stream = await liveLogs(page);
	mock.on('getLogHistory', history(Array.from({ length: 200 }, (_, i) => logLine(i + 1))));
	mock.on('getLogCatalog', { status: 403, body: { error: { code: 'forbidden', message: 'no' } } });
	await go('/logs');
	const list = page.getByRole('listbox', { name: 'Log lines' });
	await expect(page.getByText('message 200')).toBeVisible();
	await stream.opened();
	await list.hover();
	await page.mouse.wheel(0, -600);
	const top = await list.evaluate((el) => el.scrollTop);
	await stream.send([logLine(201)]);
	await expect(page.getByRole('button', { name: /1 new lines/ })).toBeVisible({ timeout: 10_000 });
	expect(await list.evaluate((el) => el.scrollTop)).toBe(top);
	await page.getByRole('button', { name: /Jump to latest/ }).click();
	await expect(page.getByText('message 201')).toBeVisible({ timeout: 10_000 });
});
