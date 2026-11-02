// Settings → Blocklist subscriptions (S-blocklists) against the OpenAPI mock, by keyboard alone,
// with axe: nothing is connected by default; connecting previews first and shows the strictness,
// with no password; removing a list asks for the password and shows the preview's exact counts.
// Waits are conditions only.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Subscriptions = components['schemas']['Subscriptions'];
type Assessment = components['schemas']['Assessment'];

const figures = {
	categories: ['anti_p2p'],
	floor: false,
	hashes: null,
	ipv4_public_share: 0.31,
	ipv4_share: 0.2,
	ipv6_ranges: 0,
	kind: 'ip',
	measured: true,
	name: 'BT_BlockLists'
};
const strictness = (level: string, text: string) => ({
	categories: ['anti_p2p'],
	estimate: false,
	floor_hashes: 0,
	hashes: 0,
	ipv4_public_share: 0.31,
	ipv4_share: 0.2,
	ipv6_ranges: 0,
	ipv6_share: 0,
	level,
	lists: [],
	text,
	unknown: []
});
const catalog = (subscription: number | null) => ({
	about: 'Addresses seen sharing for anti-P2P outfits.',
	figures,
	format: 'p2p',
	key: 'bt_blocklists',
	kind: 'ip',
	name: 'BT_BlockLists',
	subscription,
	url: 'https://lists.example.org/bt.p2p.gz'
});
const none = (): Subscriptions => ({
	catalog: [catalog(null)],
	connected: [],
	strictness: strictness('none', '')
});
const mine = (): Subscriptions => ({
	catalog: [catalog(5)],
	connected: [
		{
			figures: { ...figures, measured: false },
			held: false,
			subscription: {
				action: 'drop',
				checked_at: 1,
				enabled: true,
				entries: 100,
				error: null,
				floor: false,
				format: 'p2p',
				id: 5,
				kind: 'ip',
				name: 'BT_BlockLists',
				status: 'ok',
				updated_at: 1_700_000_000,
				url: 'https://lists.example.org/bt.p2p.gz'
			}
		}
	],
	strictness: strictness('severe', 'Severe: blocks about a third of the public internet.')
});
const preview = { caught_30d: 0, core_caught_30d: 0, per_day: 0, estimate: false, stored: 0 };
const connecting: Assessment = {
	weakening: false,
	confirm: false,
	warning: null,
	neutralize: null,
	preview: { ...preview, quarantine: 0, tombstones: 0, removed_from_floor: 0 },
	strictness: strictness('severe', 'Severe: blocks about a third of the public internet.')
};
const removing: Assessment = {
	weakening: true,
	confirm: true,
	warning: 'Removing BT_BlockLists lets 3 held and 17 blocked torrents through.',
	neutralize: null,
	preview: { ...preview, quarantine: 3, tombstones: 17, removed_from_floor: 0 },
	strictness: strictness('none', '')
};

test.describe('keyboard: Settings → Blocklist subscriptions', () => {
	let subs: Subscriptions;
	let assessment: Assessment;
	let applied: string[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend().attach(mock as unknown as MockLike);
		subs = none();
		assessment = connecting;
		applied = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getFilterSubscriptions', () => ({ status: 200, body: subs }));
		on('startFilterPreviewJob', () => ({ status: 202, body: { job: 1 } }));
		on('getFilterPreviewJob', () => ({
			status: 200,
			body: {
				state: 'done',
				progress: { phase: 'done', done: 1, total: 1 },
				assessment
			}
		}));
		on('cancelFilterPreviewJob', { status: 200, body: { ok: true } });
		on('applyFilterChange', () => {
			applied.push('apply');
			return { status: 200, body: { applied: true, assessment, version: 3, id: 5 } };
		});
	});

	test('connects nothing by default, and passes axe', async ({ page, go }) => {
		await go('/settings/blocklists');
		await expect(
			page.getByRole('heading', { level: 1, name: 'Blocklist subscriptions' })
		).toBeVisible();
		await expect(page.getByTestId('blocklists-empty')).toBeVisible();
		await expect(page.getByTestId('catalog-bt_blocklists')).toBeVisible();
		expect(applied).toEqual([]);
		await expectNoA11yViolations(page);
	});

	test('connecting shows the strictness first and asks for no password', async ({
		page,
		go,
		kbd
	}) => {
		await go('/settings/blocklists');
		await kbd.tabTo(page.getByTestId('catalog-bt_blocklists-connect'), 100);
		await kbd.press('Enter');
		await expect(page.getByTestId('blocklists-level')).toHaveText('Severe');
		await expect(page.getByTestId('blocklists-strictness-text')).toContainText('third');
		await expect(page.locator('input[name="password"]')).toHaveCount(0);
		expect(applied).toEqual([]);
		subs = mine();
		await kbd.tabTo(page.getByTestId('change-apply'), 20);
		await kbd.press('Enter');
		await expect.poll(() => applied).toEqual(['apply']);
	});

	test('removing asks for the password and shows the exact counts', async ({ page, go, kbd }) => {
		subs = mine();
		assessment = removing;
		await go('/settings/blocklists');
		await kbd.tabTo(page.getByTestId('list-5-remove'), 100);
		await kbd.press('Enter');
		await expect(page.getByTestId('effect-warning-text')).toContainText('3 held and 17 blocked');
		await expect(page.locator('input[name="password"]')).toBeVisible();
		expect(applied).toEqual([]);
	});
});
