// M-filters-dialogs, keyboard only: each dialog opened, completed and cancelled without a pointer;
// Esc closes and focus returns to the button that opened it.
import type { components } from '../src/lib/api/schema';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';

type Assessment = components['schemas']['Assessment'];

const PATH = '/dev/filters-flows';
const preview = {
	quarantine: 3,
	tombstones: 17,
	stored: 120,
	removed_from_floor: 0,
	caught_30d: 214,
	core_caught_30d: 9,
	per_day: 7,
	estimate: false
};
const weakening: Assessment = {
	weakening: true,
	confirm: true,
	warning: 'Turning off CS-4a stops that child-safety check for every torrent.',
	preview,
	neutralize: null
};
const strengthening: Assessment = { ...weakening, weakening: false, confirm: false, warning: null };

function answer(mock: OpenApiMock, assessment: Assessment) {
	mock.on('startFilterPreviewJob', { status: 202, body: { job: 1 } });
	mock.on('getFilterPreviewJob', {
		state: 'done',
		progress: { phase: 'done', done: 1, total: 1 },
		assessment
	});
	mock.on('cancelFilterPreviewJob', { ok: true });
	mock.on('applyFilterChange', { applied: true, assessment, version: 3 });
}

test('a weakening change, completed from the keyboard', async ({ page, go, mock, kbd }) => {
	answer(mock, weakening);
	await go(PATH);
	const invoker = page.getByTestId('case-core-off');
	await kbd.tabTo(invoker);
	await kbd.press('Enter');
	const field = page.getByLabel('Password');
	await expect(field).toBeFocused();
	await page.keyboard.type('keyboard-password');
	await kbd.press('Enter');
	await expect(page.getByText('What changed')).toBeVisible();
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(invoker).toBeFocused();
	await expect(page.getByTestId('last-applied')).toHaveText('Last applied: core-off v3');
});

test('a strengthening change: Tab to Apply, Enter', async ({ page, go, mock, kbd }) => {
	answer(mock, strengthening);
	await go(PATH);
	await kbd.tabTo(page.getByTestId('case-add-rule'));
	await kbd.press('Enter');
	await expect(page.getByTestId('change-dialog')).toHaveAttribute('data-state', 'review');
	await kbd.tabTo(page.getByTestId('change-apply'), 10);
	await kbd.press('Enter');
	await expect(page.getByText('What changed')).toBeVisible();
});

test('Esc cancels a change and returns focus', async ({ page, go, mock, kbd }) => {
	answer(mock, weakening);
	await go(PATH);
	const invoker = page.getByTestId('case-release');
	await kbd.tabTo(invoker);
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(invoker).toBeFocused();
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
});

test('Shadow for 7 days re-previews the shadowed variant', async ({ page, go, mock, kbd }) => {
	answer(mock, strengthening);
	await go(PATH);
	await kbd.tabTo(page.getByTestId('case-add-rule'));
	await kbd.press('Enter');
	await kbd.tabTo(page.getByTestId('change-shadow'), 10);
	await kbd.press('Enter');
	await expect(page.getByTestId('change-shadow-note')).toBeVisible();
	expect(JSON.parse(mock.calls('startFilterPreviewJob')[1]!.postData()!).change.shadow_days).toBe(
		7
	);
	await expect(page.getByTestId('change-apply')).toHaveText('Shadow for 7 days');
});

test('the 18+ dialog: Cancel has focus; Esc cancels; Enter on Show confirms', async ({
	page,
	go,
	mock,
	kbd
}) => {
	mock.on('patchSettings', { status: 200, body: {} });
	await go(PATH);
	const on = page.getByTestId('adult-on');
	await kbd.tabTo(on);
	await kbd.press('Enter');
	await expect(page.getByTestId('adult-cancel')).toBeFocused();
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(on).toBeFocused();
	await expect(page.getByTestId('adult-status')).toHaveText('Cancelled');
	await kbd.press('Enter');
	await expect(page.getByTestId('adult-cancel')).toBeFocused();
	await kbd.press('Tab');
	await expect(page.getByTestId('adult-show')).toBeFocused();
	await kbd.press('Enter');
	await expect(page.getByTestId('adult-status')).toHaveText('Adult content shown');
});

test('the override editor: type, preview, cancel with Esc', async ({ page, go, mock, kbd }) => {
	answer(mock, weakening);
	await go(PATH);
	const invoker = page.getByTestId('open-override');
	await kbd.tabTo(invoker);
	await kbd.press('Enter');
	// The dialog's chunk loads on first use: wait for it before tabbing on.
	await expect(page.getByTestId('override-form')).toBeVisible();
	await kbd.tabTo(page.getByTestId('override-value'), 15);
	await page.keyboard.type('jailbait');
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(invoker).toBeFocused();
});

test('Block this from the keyboard: hide this torrent, apply', async ({ page, go, mock, kbd }) => {
	answer(mock, strengthening);
	mock.on('searchGet', {
		canonical: '',
		hits: [],
		limit: 1,
		next_cursor: null,
		notices: [],
		offset: 0,
		stemmed: false,
		took_ms: 1,
		total: 1,
		total_approx: false
	});
	await go(PATH);
	await kbd.tabTo(page.getByTestId('open-block'));
	await kbd.press('Enter');
	await expect(page.getByTestId('block-form')).toBeVisible();
	await kbd.tabTo(page.getByTestId('block-preview'), 20);
	await kbd.press('Enter');
	await expect(page.getByTestId('change-dialog')).toHaveAttribute('data-state', 'review');
	await kbd.tabTo(page.getByTestId('change-apply'), 10);
	await kbd.press('Enter');
	await expect(page.getByText('What changed')).toBeVisible();
	await kbd.press('Escape');
	await expect(page.getByTestId('open-block')).toBeFocused();
});

test('reveal from the keyboard, then Esc', async ({ page, go, mock, kbd }) => {
	answer(mock, weakening);
	mock.on('applyFilterChange', {
		applied: true,
		assessment: weakening,
		version: 3,
		name: 'A.Name'
	});
	await go(PATH);
	const invoker = page.getByTestId('open-reveal');
	await kbd.tabTo(invoker);
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	await page.keyboard.type('keyboard-password');
	await kbd.press('Enter');
	await expect(page.getByTestId('reveal-name')).toHaveText('A.Name');
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(invoker).toBeFocused();
});

test('the override editor completed: arrow keys pick the strength, Enter applies', async ({
	page,
	go,
	mock,
	kbd
}) => {
	const neutral = { ...preview, quarantine: 1 };
	let n = 0;
	const sent: Record<string, unknown>[] = [];
	mock.on('startFilterPreviewJob', (req) => {
		sent.push(JSON.parse(req.postData() ?? '{}').change);
		return { status: 202, body: { job: ++n } };
	});
	mock.on('getFilterPreviewJob', () => ({
		state: 'done' as const,
		progress: { phase: 'done' as const, done: 1, total: 1 },
		assessment: sent.at(-1)?.neutralize
			? { ...weakening, preview: neutral }
			: { ...weakening, neutralize: neutral }
	}));
	mock.on('cancelFilterPreviewJob', { ok: true });
	mock.on('applyFilterChange', { applied: true, assessment: weakening, version: 4, id: 1 });
	await go(PATH);
	await kbd.tabTo(page.getByTestId('open-override'));
	await kbd.press('Enter');
	await expect(page.getByTestId('override-form')).toBeVisible();
	await kbd.tabTo(page.getByTestId('override-value'), 15);
	await page.keyboard.type('jailbait');
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	await kbd.tabTo(page.getByRole('radio', { name: 'Override' }), 20);
	await kbd.press('ArrowRight');
	await expect(page.getByRole('radio', { name: 'Neutralize term' })).toBeChecked();
	await expect(page.getByLabel('Password')).toBeFocused();
	expect(sent.at(-1)).toEqual({ op: 'add_override', keyword: 'jailbait', neutralize: true });
	await page.keyboard.type('keyboard-password');
	await kbd.press('Enter');
	await expect(page.getByText('What changed')).toBeVisible();
	expect(JSON.parse(mock.calls('applyFilterChange')[0]!.postData()!).change.neutralize).toBe(true);
});
