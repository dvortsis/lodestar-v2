// The shell applies the account's saved appearance on a browser that has none cached. Name ends in
// `routes.test.ts` so the `core` project runs it.
import { expectNoA11yViolations } from './a11y';
import { liveEvents } from './support/events-server';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

const field = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'db' as const
});

test('a new browser shows the saved theme and density, and caches the theme', async ({
	page,
	go,
	mock
}) => {
	await liveEvents(page);
	mock.on('getSettings', {
		section: 'preferences',
		revision: 3,
		restart_required: [],
		fields: {
			theme: field('nord'),
			theme_dark: field(''),
			mode: field('light'),
			density: field('compact'),
			motion: field('system'),
			illustrations: field(true),
			shortcuts: field(true),
			shortcut_keys: field({})
		}
	});
	await go('/search');
	const html = page.locator('html');
	await expect(html).toHaveAttribute('data-theme', 'nord-light');
	await expect(html).toHaveAttribute('data-density', 'compact');
	await expect
		.poll(() =>
			page.evaluate(() => JSON.parse(localStorage.getItem('lodestar.theme') ?? '{}').light?.id)
		)
		.toBe('nord-light');
	await expectNoA11yViolations(page);
});
