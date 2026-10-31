// S-filters, keyboard only: Settings → Filters & Safety. A loosening action asks for the password
// and Cancel sends nothing; the locked floor has no control; held adult items stay out of view
// while 18+ is off. Every state is checked with axe.
import { expectNoA11yViolations } from './a11y';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';

const H = 'a'.repeat(40);
const HA = 'b'.repeat(40);
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
const weakening = {
	weakening: true,
	confirm: true,
	warning: 'Turning off CS-4a stops that child-safety check for every torrent.',
	preview,
	neutralize: null
};

function serve(mock: OpenApiMock) {
	mock.on('getFilters', {
		allow: [],
		core: [
			{ id: 'CS-1a', on: true, floor: true, action: 'drop' },
			{ id: 'CS-4a', on: true, floor: false, action: 'quarantine' }
		],
		cs1_entries: 12,
		exceptions: [],
		ip_ranges: 0,
		overrides: [],
		packs: [],
		pending_floor_removals: [],
		quarantine: 2,
		rules: [],
		subscriptions: [],
		version: 4
	});
	mock.on('getFilterQueue', {
		decisions_30d: [],
		next_cursor: null,
		quarantine: [
			{ hash: H, rule: 'CS-4a', at: 1, expires_at: 2, sealed: true, adult_hold: false },
			{ hash: HA, rule: 'CS-5a', at: 1, expires_at: 2, sealed: true, adult_hold: true }
		]
	});
	mock.on('getSettings', {
		section: 'content',
		restart_required: [],
		revision: 1,
		fields: {
			show_adult: {
				value: false,
				env: null,
				read_only: false,
				restart_required: false,
				source: 'default'
			}
		}
	});
	mock.on('startFilterPreviewJob', { status: 202, body: { job: 1 } });
	mock.on('getFilterPreviewJob', {
		state: 'done',
		progress: { phase: 'done', done: 1, total: 1 },
		assessment: weakening
	});
	mock.on('cancelFilterPreviewJob', { ok: true });
}

test('floor has no control, adult holds stay hidden, and the screen has no axe violations', async ({
	page,
	go,
	mock
}) => {
	serve(mock);
	await go('/settings/filters');
	await expect(page.getByRole('heading', { level: 1, name: 'Filters & Safety' })).toBeVisible();
	await expect(page.getByTestId('core-switch-CS-4a')).toBeVisible();
	await expect(page.getByTestId('core-CS-1a').getByRole('button')).toHaveCount(0);
	await expect(page.getByTestId(`queue-${H}`)).toBeVisible();
	await expect(page.getByTestId(`queue-${HA}`)).toHaveCount(0);
	await expectNoA11yViolations(page);
});

test('turn off a core rule: preview, password, Esc sends nothing', async ({
	page,
	go,
	mock,
	kbd
}) => {
	serve(mock);
	await go('/settings/filters');
	const invoker = page.getByTestId('core-switch-CS-4a');
	await kbd.tabTo(invoker, 40);
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	await expectNoA11yViolations(page);
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(invoker).toBeFocused();
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
});

test('release a held torrent from the keyboard: the dialog asks first', async ({
	page,
	go,
	mock,
	kbd
}) => {
	serve(mock);
	await go('/settings/filters');
	await kbd.tabTo(page.getByTestId(`queue-release-${H}`), 60);
	await kbd.press('Enter');
	await expect(page.getByLabel('Password')).toBeFocused();
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
	await kbd.press('Escape');
	await expect(page.getByRole('dialog')).toHaveCount(0);
	expect(mock.calls('applyFilterChange')).toHaveLength(0);
});
