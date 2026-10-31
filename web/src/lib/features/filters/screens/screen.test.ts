// Safety invariants of the Filters & Safety screen (S-filters). The wire is a scripted fetch.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import FiltersScreen from './FiltersScreen.svelte';
import { adultOn } from './model.svelte';
import { mockFetch } from '../flows/testing/fetch';
import { finished, strengthening, weakening } from '../flows/testing/fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });

const H = 'a'.repeat(40);
const HA = 'b'.repeat(40);
const overview = (over: Record<string, unknown> = {}) => ({
	allow: [{ id: 2, query: 'type:movie' }],
	core: [
		{ id: 'CS-1a', on: true, floor: true, action: 'drop' },
		{ id: 'CS-4a', on: true, floor: false, action: 'quarantine' }
	],
	cs1_entries: 12,
	exceptions: [{ id: 3, rule: 'CS-4a', hashes: [H] }],
	ip_ranges: 0,
	overrides: [
		{ id: 8, keyword: 'x', strength: 'override', scope: null, expires_at: null, shadow_until: null }
	],
	packs: [{ id: 'malware', on: true, rules: [] }],
	pending_floor_removals: [
		{ source: 'file:/a', kind: 'file', name: '/a', entries: 2, hashes: [H] }
	],
	quarantine: 2,
	rules: [
		{ id: 5, query: 'name:"x"', action: 'hide', enabled: true, shadow_until: null, unless: null }
	],
	subscriptions: [],
	version: 4,
	...over
});
const queue = {
	decisions_30d: [{ rule: 'CS-5a', action: 'quarantine', count: 37, shadow: false }],
	next_cursor: null,
	quarantine: [
		{
			hash: H,
			rule: 'CS-4a',
			at: 1,
			expires_at: 2,
			sealed: true,
			adult_hold: false,
			name: 'SECRET-NAME'
		},
		{
			hash: HA,
			rule: 'CS-5a',
			at: 1,
			expires_at: 2,
			sealed: true,
			adult_hold: true,
			name: 'SECRET-NAME'
		}
	]
};
const content = (show: boolean) => ({ revision: 1, fields: { show_adult: { value: show } } });

let net: ReturnType<typeof mockFetch>;
function setup(adult = false, over: Record<string, unknown> = {}) {
	net = mockFetch();
	net.on('GET /filters', { body: overview(over) });
	net.on('GET /filters/queue', { body: queue });
	net.on('GET /settings/content', { body: content(adult) });
	net.on('POST /filters/preview/jobs', { status: 202, body: { job: 7 } });
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
	return render(FiltersScreen);
}
const job = (a: ReturnType<typeof weakening>) =>
	net.on('GET /filters/preview/jobs/{id}', { body: finished(a) });
afterEach(() => vi.unstubAllGlobals());
beforeEach(() => vi.useRealTimers());

describe('locked floor', () => {
	it('has no control, while a normal core rule does', async () => {
		setup();
		await screen.findByTestId('core-CS-1a');
		expect(screen.queryByTestId('core-switch-CS-1a')).toBeNull();
		expect(screen.getByTestId('core-CS-1a').querySelector('button')).toBeNull();
		expect(screen.getByTestId('core-switch-CS-4a')).toBeTruthy();
	});
	it('ignores URL parameters', async () => {
		history.replaceState(null, '', '/settings/filters?off=CS-1a&rule=CS-1a&on=0');
		setup();
		await screen.findByTestId('core-CS-1a');
		expect(net.to('/filters/apply')).toHaveLength(0);
		expect(net.to('/filters/preview')).toHaveLength(0);
		history.replaceState(null, '', '/');
	});
});

describe('loosening actions and Cancel', () => {
	const cases: [string, string][] = [
		['core off', 'core-switch-CS-4a'],
		['pack off', 'pack-switch-malware'],
		['rule disable', 'rule-toggle-5'],
		['remove exception', 'exception-remove-3'],
		['remove allow', 'allow-remove-2']
	];
	for (const [name, id] of cases)
		it(`${name}: previews first, Cancel sends no apply`, async () => {
			setup();
			job(weakening());
			await fireEvent.click(await screen.findByTestId(id));
			await screen.findByTestId('change-cancel');
			expect(net.to('/filters/apply')).toHaveLength(0);
			await fireEvent.click(screen.getByTestId('change-cancel'));
			await waitFor(() => expect(screen.queryByTestId('change-dialog')).toBeNull());
			expect(net.to('/filters/apply')).toHaveLength(0);
		});
	const more: [string, string][] = [
		['release', `queue-release-${H}`],
		['false positive', `queue-fp-${H}`],
		['confirm', `queue-confirm-${H}`],
		['remove override', 'override-remove-8'],
		['remove rule', 'rule-remove-5'],
		['floor removal', 'pending-remove-file:/a'],
		['undo', 'filters-undo']
	];
	for (const [name, id] of more)
		it(`${name}: previews first, Cancel sends no apply`, async () => {
			setup(true);
			job(weakening());
			await fireEvent.click(await screen.findByTestId(id));
			await screen.findByTestId('change-password');
			expect(net.to('/filters/preview/jobs').length).toBeGreaterThan(0);
			await fireEvent.click(screen.getByTestId('change-cancel'));
			await waitFor(() => expect(screen.queryByTestId('change-dialog')).toBeNull());
			expect(net.to('/filters/apply')).toHaveLength(0);
		});
	it('add Always allow: previews first, Cancel sends no apply', async () => {
		setup();
		job(weakening());
		await screen.findByTestId('allow-add');
		await fireEvent.input(document.querySelector('input[name="allow-query"]')!, {
			target: { value: 'type:movie' }
		});
		await fireEvent.click(screen.getByTestId('allow-add'));
		await screen.findByTestId('change-password');
		await fireEvent.click(screen.getByTestId('change-cancel'));
		await waitFor(() => expect(screen.queryByTestId('change-dialog')).toBeNull());
		expect(net.to('/filters/apply')).toHaveLength(0);
	});
	it('a weakening change asks for the password before any apply', async () => {
		setup();
		job(weakening());
		await fireEvent.click(await screen.findByTestId('core-switch-CS-4a'));
		await screen.findByTestId('change-password');
		expect(net.to('/filters/apply')).toHaveLength(0);
	});
});

describe('a failed request keeps the safer state', () => {
	it('a refused turn-off leaves the rule On', async () => {
		setup();
		job(weakening());
		net.error('POST /filters/apply', 500, 'boom');
		await fireEvent.click(await screen.findByTestId('core-switch-CS-4a'));
		const pw = await screen.findByTestId('change-password');
		await fireEvent.input(pw, { target: { value: 'pw' } });
		await fireEvent.click(screen.getByTestId('change-apply'));
		await screen.findByTestId('change-retry');
		const row = screen.getByTestId('core-CS-4a');
		expect(row.textContent).toContain('On');
		expect(row.textContent).not.toContain('Off');
	});
	it('a failed reload after an apply says the screen may be out of date', async () => {
		setup();
		job(weakening());
		net.on('POST /filters/apply', { body: { applied: true, assessment: weakening(), version: 5 } });
		await fireEvent.click(await screen.findByTestId('core-switch-CS-4a'));
		await fireEvent.input(await screen.findByTestId('change-password'), {
			target: { value: 'pw' }
		});
		net.error('GET /filters', 500, 'down');
		await fireEvent.click(screen.getByTestId('change-apply'));
		await screen.findByTestId('filters-stale');
	});
	it('an unreadable overview shows an error and no controls', async () => {
		net = mockFetch();
		net.error('GET /filters', 500, 'down');
		net.on('GET /settings/content', { body: content(false) });
		render(FiltersScreen);
		await screen.findByTestId('filters-failed');
		expect(screen.queryByTestId('core-switch-CS-4a')).toBeNull();
	});
});

describe('held items and adult', () => {
	it('adult holds are absent while 18+ is off, and nothing says they exist', async () => {
		setup(false);
		await screen.findByTestId(`queue-${H}`);
		expect(screen.queryByTestId(`queue-${HA}`)).toBeNull();
		expect(document.body.textContent).not.toContain('CS-5a');
		expect(screen.queryByTestId('filters-decisions')).toBeNull();
	});
	it('a page of only adult holds with more to come never says nothing is waiting', async () => {
		setup(false);
		net.on(
			'GET /filters/queue',
			{
				body: {
					decisions_30d: [],
					next_cursor: 'c1',
					quarantine: [
						{ hash: HA, rule: 'CS-5a', at: 1, expires_at: 2, sealed: true, adult_hold: true }
					]
				}
			},
			{ body: queue }
		);
		await screen.findByTestId(`queue-${H}`);
		expect(screen.queryByTestId(`queue-${HA}`)).toBeNull();
	});
	it('an unreadable setting counts as off', async () => {
		setup();
		net.error('GET /settings/content', 500, 'x');
		await screen.findByTestId(`queue-${H}`);
		expect(screen.queryByTestId(`queue-${HA}`)).toBeNull();
	});
	it('adult holds show when 18+ is on', async () => {
		setup(true);
		await screen.findByTestId(`queue-${HA}`);
	});
	it('only a clear true reads as on', () => {
		for (const v of [false, 'true', 1, null, undefined])
			expect(adultOn({ fields: { show_adult: { value: v } } })).toBe(false);
		expect(adultOn(null)).toBe(false);
		expect(adultOn({ fields: { show_adult: { value: true } } })).toBe(true);
	});
	it('the queue renders hashes only: no name, no image, until Reveal is used', async () => {
		setup(true);
		await screen.findByTestId(`queue-${H}`);
		expect(screen.getByTestId('filters-queue').querySelector('img')).toBeNull();
		expect(net.to('/filters/apply')).toHaveLength(0);
		expect(screen.queryByTestId('reveal-name')).toBeNull();
		expect(document.body.textContent).not.toContain('SECRET-NAME');
	});
});

describe('adding a rule', () => {
	it('an empty query sends nothing', async () => {
		setup();
		await fireEvent.click(await screen.findByTestId('rule-add'));
		expect(net.to('/filters/preview')).toHaveLength(0);
	});
	it('a strengthening rule previews and needs no password', async () => {
		setup();
		job(strengthening());
		await screen.findByTestId('rule-add');
		await fireEvent.input(document.querySelector('input[name="rule-query"]')!, {
			target: { value: 'name:"bad"' }
		});
		await fireEvent.click(screen.getByTestId('rule-add'));
		await screen.findByTestId('change-apply');
		expect(screen.queryByTestId('change-password')).toBeNull();
	});
});
