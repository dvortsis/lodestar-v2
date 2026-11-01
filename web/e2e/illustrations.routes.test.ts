// X-illustrations: the Appearance toggle and the browser's data saver both hide state illustrations,
// and no scene image is requested (§10.19).
import type { Page } from '@playwright/test';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

const field = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const
});
const prefs = (illustrations: boolean) => ({
	section: 'preferences',
	revision: 3,
	restart_required: [],
	fields: {
		theme: field('riverstone'),
		theme_dark: field(''),
		mode: field('light'),
		density: field('comfortable'),
		motion: field('system'),
		illustrations: field(illustrations)
	}
});

const sceneRequests = (page: Page) => {
	const seen: string[] = [];
	page.on('request', (r) => {
		if (r.resourceType() === 'image' && /\.(avif|webp)(\?|$)/.test(r.url())) seen.push(r.url());
	});
	return seen;
};

async function expectNoScene(page: Page, seen: string[]) {
	await expect(page.getByRole('heading', { level: 1, name: 'Page not found' })).toBeVisible();
	await expect(page.locator('[data-testid="state-screen"] img')).toHaveCount(0);
	expect(seen).toEqual([]);
}

test('illustrations off in Appearance: no scene is rendered or fetched', async ({
	page,
	go,
	mock
}) => {
	mock.on('getSettings', prefs(false));
	await page.addInitScript(() => {
		localStorage.setItem(
			'lodestar.appearance',
			JSON.stringify({
				density: 'comfortable',
				motion: 'system',
				illustrations: false
			})
		);
	});
	const seen = sceneRequests(page);
	await go('/definitely-not-a-page');
	await expectNoScene(page, seen);
});

test('Save-Data hides illustrations whatever the stored preference', async ({ page, go, mock }) => {
	mock.on('getSettings', prefs(true));
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'connection', { value: { saveData: true } });
	});
	const seen = sceneRequests(page);
	await go('/definitely-not-a-page');
	await expectNoScene(page, seen);
});
