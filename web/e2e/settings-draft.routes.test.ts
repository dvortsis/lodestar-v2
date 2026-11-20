// X-session-draft-keep (UI review L2) on /dev/settings-form against the OpenAPI mock: a save that
// finds the session ended says so, keeps the draft across the sign-in and restores it on return;
// staying on the page asks again on the next save; the leave dialog's "Save changes" saves.
// Condition waits only, each with a deadline of at least 10 s.
import { expect, test } from './support/test';
import { SettingsBackend } from '../src/lib/features/settings/form/testing/e2e-backend';

const PAGE = '/dev/settings-form';
const WAIT = { timeout: 10_000 };
/** The page, whatever query the harness adds (`?density=…`). */
const ON_PAGE = new RegExp(`${PAGE}(\\?|$)`);

const envelope = (code: string) => ({ error: { code, message: code } });
const loginRequired = {
	status: 401,
	body: { ...envelope('login_required'), setup_required: false }
};

test.describe('a Settings draft across sign-in', () => {
	let backend: SettingsBackend;
	let session: boolean;
	test.beforeEach(async ({ mock }) => {
		backend = new SettingsBackend().attach(mock);
		session = true;
		mock.on('getAuthStatus', () => ({
			status: 200,
			body: { logged_in: session, notice_required: false, setup_required: false }
		}));
		mock.on('login', () => {
			session = true;
			return { status: 200, body: { ok: true } };
		});
	});

	const ready = async (page: import('@playwright/test').Page) => {
		await expect(page.getByRole('heading', { level: 1, name: 'Settings form' })).toBeVisible(WAIT);
		await expect(page.locator('[data-field="crawler.intensity"] [role="radiogroup"]')).toBeVisible(
			WAIT
		);
	};

	test('says the session ended, keeps the change and restores it after sign-in', async ({
		page,
		go
	}) => {
		await go(PAGE);
		await ready(page);
		await page.getByRole('radio', { name: 'aggressive' }).check();
		const bar = page.getByTestId('save-bar');
		await expect(bar).toContainText('1 unsaved change', WAIT);

		// The session ends: the save is refused with a 401.
		session = false;
		backend.nextPatch.push(loginRequired);
		await bar.getByRole('button', { name: 'Save changes' }).click();
		const dialog = page.getByRole('dialog', { name: "You've been signed out" });
		await expect(dialog).toBeVisible(WAIT);
		await expect(dialog).toContainText("Your changes aren't saved yet (1)");
		await expect(dialog).toContainText('This browser tab keeps them');
		// Never the generic leave question.
		await expect(page.getByRole('dialog', { name: 'Leave without saving?' })).toHaveCount(0);

		await dialog.getByRole('button', { name: 'Sign in' }).click();
		await expect(page).toHaveURL(/\/login\?next=/, WAIT);
		expect(new URL(page.url()).searchParams.get('next')).toMatch(ON_PAGE);
		const stored = await page.evaluate(() => ({
			draft: sessionStorage.getItem('lodestar.settings.draft.v1.showcase'),
			kept: sessionStorage.getItem('lodestar.settings.kept.v1')
		}));
		expect(stored.draft).toContain('aggressive');
		expect(stored.kept).toContain(PAGE);

		await page.getByLabel('Password').fill('correct horse battery');
		await page.getByRole('button', { name: 'Sign in' }).click();
		await expect(page).toHaveURL(ON_PAGE, WAIT);
		await ready(page);
		// Restored at once, with a line that says it isn't saved yet: no Restore question.
		await expect(page.getByRole('radio', { name: 'aggressive' })).toBeChecked(WAIT);
		await expect(page.getByTestId('draft-kept')).toContainText("they aren't saved yet");
		await expect(page.getByRole('button', { name: 'Restore' })).toHaveCount(0);
		await expect(bar).toContainText('1 unsaved change');

		await bar.getByRole('button', { name: 'Save changes' }).click();
		await expect(bar).toContainText('Saved', WAIT);
		expect(backend.patches.at(-1)).toEqual({
			crawler: { if_revision: 3, intensity: 'aggressive' }
		});
		await expect(page.getByTestId('draft-kept')).toHaveCount(0);
	});

	test('Keep editing stays, and the next save that finds the session ended asks again', async ({
		page,
		go
	}) => {
		await go(PAGE);
		await ready(page);
		await page.getByRole('radio', { name: 'low' }).check();
		const bar = page.getByTestId('save-bar');
		await expect(bar).toContainText('1 unsaved change', WAIT);

		session = false;
		backend.nextPatch.push(loginRequired, loginRequired);
		await bar.getByRole('button', { name: 'Save changes' }).click();
		const dialog = page.getByRole('dialog', { name: "You've been signed out" });
		await expect(dialog).toBeVisible(WAIT);
		await dialog.getByRole('button', { name: 'Keep editing' }).click();
		await expect(dialog).toHaveCount(0, WAIT);
		await expect(page).toHaveURL(ON_PAGE);
		await expect(page.getByRole('radio', { name: 'low' })).toBeChecked();
		expect(
			await page.evaluate(() => sessionStorage.getItem('lodestar.settings.kept.v1'))
		).toBeNull();

		await bar.getByRole('button', { name: 'Save changes' }).click();
		await expect(dialog).toBeVisible(WAIT);
	});

	test('the leave dialog’s Save changes saves, then leaves', async ({ page, go }) => {
		await go(PAGE);
		await ready(page);
		await page.getByRole('radio', { name: 'low' }).check();
		await page.getByRole('link', { name: 'Another page' }).click();
		const dialog = page.getByRole('dialog', { name: 'Leave without saving?' });
		await expect(dialog).toBeVisible(WAIT);
		await dialog.getByRole('button', { name: 'Save changes' }).click();
		await expect(page).toHaveURL(/\/dev\/ui\/controls/, WAIT);
		expect(backend.patches).toEqual([{ crawler: { if_revision: 3, intensity: 'low' } }]);
	});

	test('a failed save from the leave dialog stays, with the error', async ({ page, go }) => {
		await go(PAGE);
		await ready(page);
		await page.getByRole('radio', { name: 'low' }).check();
		backend.nextPatch.push({ status: 500, body: envelope('internal') });
		await page.getByRole('link', { name: 'Another page' }).click();
		const dialog = page.getByRole('dialog', { name: 'Leave without saving?' });
		await dialog.getByRole('button', { name: 'Save changes' }).click();
		await expect(dialog).toHaveCount(0, WAIT);
		await expect(page).toHaveURL(ON_PAGE);
		await expect(page.getByText("Lodestar couldn't save the change.")).toBeVisible(WAIT);
		await expect(page.getByRole('radio', { name: 'low' })).toBeChecked();
	});
});
