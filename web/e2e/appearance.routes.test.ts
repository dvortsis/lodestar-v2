import { expect as baseExpect, test } from './support/test';
import { expectNoA11yViolations } from './a11y';

const expect = baseExpect.configure({ timeout: 10_000 });

const field = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const
});
const sections = (show: boolean) => (request: Request) =>
	request.url().includes('/settings/content')
		? {
				section: 'content',
				revision: 2,
				restart_required: [],
				fields: { show_adult: field(show) }
			}
		: {
				section: 'preferences',
				revision: 4,
				restart_required: [],
				fields: {
					theme: field('riverstone'),
					theme_dark: field(''),
					mode: field('light'),
					density: field('comfortable'),
					motion: field('system'),
					illustrations: field(true)
				}
			};

for (const width of [390, 1366]) {
	test(`Appearance picks a theme and applies it at once at ${width} px`, async ({
		page,
		go,
		mock
	}) => {
		mock.on('getSettings', sections(false));
		mock.on('patchSettings', {
			section: 'preferences',
			revision: 5,
			restart_required: [],
			fields: {}
		});
		await page.setViewportSize({ width, height: 900 });
		await go('/settings/appearance');
		await expect(page.getByRole('heading', { level: 1, name: 'Appearance' })).toBeVisible();
		await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
		await page.getByRole('button', { name: 'Browse themes' }).click();
		const group = page.getByRole('radiogroup', { name: 'Theme' });
		await expect(group).toBeVisible();
		await group.getByRole('radio', { name: /Nord/ }).check();
		await expect(page.locator('html')).toHaveAttribute('data-theme', 'nord-light');
		await expect(page.getByRole('button', { name: 'Undo' })).toBeVisible();
		await page.getByRole('radio', { name: 'Compact' }).check();
		await expect(page.locator('html')).toHaveAttribute('data-density', 'compact');
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
			true
		);
		await expectNoA11yViolations(page);
	});
}

test('Appearance asks for the 18+ confirmation before showing adult content', async ({
	page,
	go,
	mock
}) => {
	mock.on('getSettings', sections(false));
	mock.on('patchSettings', { section: 'content', revision: 3, restart_required: [], fields: {} });
	await go('/settings/appearance');
	await page.getByRole('switch', { name: 'Show adult content' }).check();
	await expect(page.getByText(/Turn it on only if you are 18 or older/)).toBeVisible();
	expect(mock.calls('patchSettings')).toHaveLength(0);
	await page.getByRole('button', { name: /18 or older/ }).click();
	await expect.poll(() => mock.calls('patchSettings').length).toBe(1);
});

test('Appearance saves a custom theme in this browser and applies it', async ({
	page,
	go,
	mock
}) => {
	mock.on('getSettings', sections(false));
	mock.on('patchSettings', {
		section: 'preferences',
		revision: 5,
		restart_required: [],
		fields: {}
	});
	await go('/settings/appearance');
	await page.getByRole('button', { name: 'Browse themes' }).click();
	await page.getByRole('button', { name: 'Create a custom theme' }).click();
	const editor = page.getByTestId('theme-editor');
	await expect(editor.getByRole('table')).toBeVisible();
	await editor.getByLabel('Theme name').fill('Harbour');
	await editor.getByRole('button', { name: 'Save theme' }).click();
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'custom-harbour-light');
	const stored = await page.evaluate(() => localStorage.getItem('lodestar.customThemes'));
	expect(JSON.parse(stored!).map((t: { id: string }) => t.id)).toEqual([
		'custom-harbour-light',
		'custom-harbour-dark'
	]);
	await expect(page.getByRole('radio', { name: /Harbour/ })).toBeChecked();
	await expectNoA11yViolations(page);
});
