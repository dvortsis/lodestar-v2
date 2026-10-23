// M-filters-dialogs: the change flows on the test-only showcase (/dev/filters-flows), answered by
// the OpenAPI mock. Named `*.routes.test.ts` so the `core` project runs it.
import type { Page, Request } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';

type Assessment = components['schemas']['Assessment'];
type Preview = components['schemas']['Preview'];
type ApplyResponse = components['schemas']['ApplyResponse'];

const PATH = '/dev/filters-flows';
const PW = 'e2e-admin-password';
const WARNING =
	'Turning off CS-4a stops that child-safety check for every torrent. Other core rules keep checking.';

const preview = (over: Partial<Preview> = {}): Preview => ({
	quarantine: 3,
	tombstones: 17,
	stored: 120,
	removed_from_floor: 0,
	caught_30d: 214,
	core_caught_30d: 9,
	per_day: 7,
	estimate: false,
	...over
});
const weakening = (over: Partial<Assessment> = {}): Assessment => ({
	weakening: true,
	confirm: true,
	warning: WARNING,
	preview: preview(),
	neutralize: null,
	...over
});
const strengthening: Assessment = {
	weakening: false,
	confirm: false,
	warning: null,
	preview: preview({
		quarantine: 0,
		tombstones: 0,
		stored: 0,
		caught_30d: 0,
		core_caught_30d: 0,
		per_day: 0
	}),
	neutralize: null
};

/** One preview job that answers `assessments` in turn (the last repeats). */
function previews(mock: OpenApiMock, ...assessments: Assessment[]) {
	let job = 0;
	mock.on('startFilterPreviewJob', () => ({ status: 202, body: { job: ++job } }));
	mock.on('getFilterPreviewJob', () => ({
		state: 'done' as const,
		progress: { phase: 'done' as const, done: 1, total: 1 },
		assessment: assessments[Math.min(job, assessments.length) - 1]!
	}));
	mock.on('cancelFilterPreviewJob', { ok: true });
}

const body = (r: Request) => r.postData() ?? '';
const dialog = (page: Page) => page.getByRole('dialog');
const settled = (page: Page) =>
	page.evaluate(() => Promise.all(document.getAnimations().map((a) => a.finished)));

test.describe('change flows', () => {
	test('a strengthening change asks for no password', async ({ page, go, mock }) => {
		previews(mock, strengthening);
		mock.on('applyFilterChange', { applied: true, assessment: strengthening, version: 4, id: 9 });
		await go(PATH);
		await page.getByTestId('case-add-rule').click();
		await expect(dialog(page).getByTestId('change-dialog')).toHaveAttribute('data-state', 'review');
		await expect(page.getByTestId('effect-warning')).toHaveCount(0);
		await expect(page.locator('input[name="password"]')).toHaveCount(0);
		await page.getByTestId('change-apply').click();
		await expect(dialog(page).getByText('What changed')).toBeVisible();
		await expect(page.getByTestId('changed-id')).toHaveText('9');
		expect(Object.keys(JSON.parse(body(mock.calls('applyFilterChange')[0]!)))).toEqual(['change']);
		await page.getByTestId('change-cancel').click();
		await expect(page.getByTestId('last-applied')).toHaveText('Last applied: add-rule v4');
	});

	test('a weakening change: warning, password, apply, What changed', async ({ page, go, mock }) => {
		previews(mock, weakening());
		mock.on('applyFilterChange', {
			applied: true,
			assessment: weakening(),
			version: 5,
			released: 3
		});
		await go(PATH);
		await page.getByTestId('case-core-off').click();
		await expect(page.getByTestId('effect-warning-text')).toHaveText(WARNING);
		await expect(page.getByText('Enter your password to apply.')).toBeVisible();
		const field = page.getByLabel('Password');
		await expect(field).toBeFocused();
		await expect(page.getByTestId('impact-quarantine')).toHaveText('3');
		await expect(page.getByTestId('impact-per_day')).toHaveText('7/d');
		await field.fill(PW);
		await field.press('Enter');
		await expect(page.getByTestId('changed-released')).toHaveText('3');
		expect(JSON.parse(body(mock.calls('applyFilterChange')[0]!))).toEqual({
			change: { op: 'core_switch', rule: 'CS-4a', on: false },
			password: PW
		});
		for (const op of ['startFilterPreviewJob', 'getFilterPreviewJob'] as const) {
			for (const r of mock.calls(op)) {
				expect(body(r)).not.toContain('password');
				expect(r.url()).not.toContain(PW);
			}
		}
		expect(page.url()).not.toContain(PW);
		const stored = await page.evaluate(() =>
			JSON.stringify([{ ...localStorage }, { ...sessionStorage }])
		);
		expect(stored).not.toContain(PW);
	});

	test('a wrong password, then the right one', async ({ page, go, mock }) => {
		previews(mock, weakening());
		let n = 0;
		mock.on('applyFilterChange', () =>
			++n === 1
				? {
						status: 401,
						body: {
							error: { code: 'bad_password', message: 'That password is wrong.', field: 'password' }
						}
					}
				: { applied: true, assessment: weakening(), version: 6 }
		);
		await go(PATH);
		await page.getByTestId('case-core-off').click();
		const field = page.getByLabel('Password');
		await field.fill('wrong');
		await field.press('Enter');
		await expect(page.getByTestId('field-error')).toContainText('That password is wrong');
		await expect(field).toHaveValue('');
		await expect(page).toHaveURL(new RegExp(`${PATH}\\?`));
		await field.fill(PW);
		await field.press('Enter');
		await expect(dialog(page).getByText('What changed')).toBeVisible();
	});

	test('rate limited: a countdown, and nothing sent until it ends', async ({ page, go, mock }) => {
		previews(mock, weakening());
		mock.on('applyFilterChange', {
			status: 429,
			headers: { 'Retry-After': '2' },
			body: { error: { code: 'rate_limited', message: 'Too many attempts.' } }
		});
		await go(PATH);
		await page.getByTestId('case-core-off').click();
		await page.getByLabel('Password').fill(PW);
		await page.getByLabel('Password').press('Enter');
		await expect(page.getByTestId('change-countdown')).toContainText('Try again in');
		await expect(page.getByTestId('change-apply')).toHaveAttribute('aria-disabled', 'true');
		await page.getByLabel('Password').fill(PW);
		await page.getByLabel('Password').press('Enter');
		expect(mock.calls('applyFilterChange')).toHaveLength(1);
		await expect(page.getByText('You can try again now.')).toBeVisible({ timeout: 5000 });
	});

	test('changed_since_preview previews again; a changed 428 says so', async ({
		page,
		go,
		mock
	}) => {
		previews(mock, strengthening, strengthening);
		let n = 0;
		mock.on('applyFilterChange', () =>
			++n === 1
				? {
						status: 409,
						body: { error: { code: 'changed_since_preview', message: 'It changed.' } }
					}
				: {
						status: 428,
						body: {
							error: { code: 'confirm_required', message: 'Confirm.', field: 'password' },
							assessment: weakening()
						}
					}
		);
		await go(PATH);
		await page.getByTestId('case-core-on').click();
		await page.getByTestId('change-apply').click();
		await expect(page.getByTestId('change-dialog')).toHaveAttribute('data-state', 'review');
		expect(mock.calls('startFilterPreviewJob')).toHaveLength(2);
		await page.getByTestId('change-apply').click();
		await expect(page.getByTestId('change-changed')).toHaveText(
			'The effect changed. Read the warning again.'
		);
		await expect(page.getByLabel('Password')).toBeFocused();
		expect(mock.calls('applyFilterChange')).toHaveLength(2);
	});

	test('a refused change shows the server message; floor rules can’t be switched off', async ({
		page,
		go,
		mock
	}) => {
		previews(mock, weakening());
		mock.error(
			'applyFilterChange',
			409,
			'floor_locked',
			'CS-1a is part of the locked floor and stays on.'
		);
		await go(PATH);
		await page.getByTestId('case-core-off').click();
		await page.getByLabel('Password').fill(PW);
		await page.getByLabel('Password').press('Enter');
		await expect(page.getByTestId('change-error')).toHaveText(
			'CS-1a is part of the locked floor and stays on.'
		);
		await expect(page.getByTestId('change-retry')).toHaveCount(0);
		await expect(page.locator('input[name="password"]')).toHaveCount(0);
	});

	test('Esc while applying waits for the answer, then closes', async ({ page, go, mock }) => {
		previews(mock, weakening());
		let answer!: () => void;
		const held = new Promise<void>((r) => (answer = r));
		// The mock awaits a function's answer; its type only describes synchronous ones.
		mock.on('applyFilterChange', (async () => {
			await held;
			return { applied: true, assessment: weakening(), version: 7 };
		}) as unknown as () => ApplyResponse);
		await go(PATH);
		await page.getByTestId('case-core-off').click();
		await page.getByLabel('Password').fill(PW);
		await page.getByLabel('Password').press('Enter');
		await expect(page.getByTestId('change-dialog')).toHaveAttribute('data-state', 'applying');
		await page.keyboard.press('Escape');
		await expect(dialog(page)).toBeVisible();
		answer();
		await expect(dialog(page)).toHaveCount(0);
		await expect(page.getByTestId('last-applied')).toHaveText('Last applied: core-off v7');
		await expect(page.getByTestId('case-core-off')).toBeFocused();
	});

	test('the 18+ confirmation', async ({ page, go, mock }) => {
		mock.on('patchSettings', { status: 200, body: {} });
		await go(PATH);
		await page.getByTestId('adult-on').click();
		const d = page.getByRole('dialog', { name: 'Show adult content?' });
		await expect(d).toBeVisible();
		await expect(page.getByTestId('adult-cancel')).toBeFocused();
		await page.getByTestId('adult-cancel').click();
		await expect(page.getByTestId('adult-status')).toHaveText('Cancelled');
		expect(mock.calls('patchSettings')).toHaveLength(0);
		await page.getByTestId('adult-on').click();
		await page.getByTestId('adult-show').click();
		await expect(page.getByTestId('adult-status')).toHaveText('Adult content shown');
		expect(JSON.parse(body(mock.calls('patchSettings')[0]!))).toEqual({
			show_adult: true,
			confirm_adult: true
		});
		mock.error('patchSettings', 409, 'set_by_environment', 'The environment variable sets this.', {
			field: 'show_adult',
			section: 'content'
		});
		await page.getByTestId('adult-off').click();
		await expect(page.getByTestId('adult-status')).toContainText('LODESTAR_SHOW_ADULT');
	});
});

for (const [themeId, colorMode] of [
	['riverstone-light', 'light'],
	['riverstone-dark', 'dark']
] as const) {
	test.describe(`axe, ${themeId}`, () => {
		test.use({ themeId, colorMode });
		for (const width of [1366, 390]) {
			test(`dialogs at ${width}`, async ({ page, go, mock }) => {
				await page.setViewportSize({ width, height: 900 });
				previews(mock, weakening({ preview: preview({ estimate: true }) }));
				mock.on('applyFilterChange', {
					applied: true,
					assessment: weakening(),
					version: 5,
					released: 3
				});
				await go(PATH);
				await expectNoA11yViolations(page);
				await page.getByTestId('case-core-off').click();
				await expect(page.getByLabel('Password')).toBeFocused();
				await settled(page);
				await expectNoA11yViolations(page);
				await page.getByLabel('Password').fill(PW);
				await page.getByLabel('Password').press('Enter');
				await expect(page.getByTestId('changed-released')).toBeVisible();
				await expectNoA11yViolations(page);
				await page.getByTestId('change-cancel').click();
				await page.getByTestId('adult-on').click();
				await expect(page.getByTestId('adult-cancel')).toBeFocused();
				await settled(page);
				await expectNoA11yViolations(page);
			});
		}
	});
}
