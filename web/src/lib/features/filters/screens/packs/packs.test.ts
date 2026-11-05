// The packs panels (RP-6): every state of a pack's version, and the acceptance and re-confirm
// dialogs. The server decides what weakens: these tests script its answers, and check that the
// screen asks for the password exactly when `confirm` says so and sends back what was previewed.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import FiltersScreen from '../FiltersScreen.svelte';
import { mockFetch } from '../../flows/testing/fetch';
import { finished, strengthening, weakening } from '../../flows/testing/fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });

const rule = (over: Record<string, unknown> = {}) => ({
	id: 'MW-2',
	on: true,
	version: 2,
	applied: 1,
	pending: null,
	action: 'hide',
	base_action: 'hide',
	base_changed: false,
	...over
});
const pack = (rules: unknown[]) => ({
	id: 'malware',
	on: true,
	version: 'MW-2@2',
	changelog: '## MW-2@2\n<b>bold</b> catches more',
	rules
});
const pend = {
	pack: 'malware',
	rule: 'MW-2',
	applied: 1,
	version: 2,
	direction: 'widens',
	changelog: [{ version: 2, direction: 'widens', text: '<img src=x onerror=alert(1)> wider' }]
};
const acceptance = (over: Record<string, unknown> = {}) => ({
	rule: 'MW-2',
	version: 2,
	applied: 1,
	direction: 'widens',
	disagreements: 0,
	heavier: 0,
	in_doubt: false,
	lighter: 0,
	newly_held: 4,
	no_longer_held: 0,
	review_quarantine: 0,
	review_tombstones: 0,
	strengthens: true,
	...over
});
const corpus = (over: Record<string, unknown> = {}) => ({
	core: [],
	packs: {
		error: null,
		failures: [],
		passes: true,
		precision_failing: 0,
		precision_items: 10,
		rules: [
			{
				pack: 'malware',
				rule: 'MW-2',
				version: 2,
				must_match: 5,
				must_not_match: 5,
				failing: 0,
				gaps: 0,
				direction_disagreements: 0
			}
		],
		...over
	}
});
const overview = (rules: unknown[], pending: unknown[] = []) => ({
	allow: [],
	core: [],
	cs1_entries: 0,
	exceptions: [],
	ip_ranges: 0,
	overrides: [],
	pack_conditions: [],
	packs: [pack(rules)],
	packs_seeded_over_rules: null,
	pending_floor_removals: [],
	pending_pack_versions: pending,
	quarantine: 0,
	rules: [],
	subscriptions: [],
	version: 4
});

let net: ReturnType<typeof mockFetch>;
function setup(rules: unknown[], pending: unknown[] = [], fixtures: unknown = corpus()) {
	net = mockFetch();
	net.on('GET /filters', { body: overview(rules, pending) });
	net.on('GET /filters/queue', { body: { decisions_30d: [], next_cursor: null, quarantine: [] } });
	net.on('GET /settings/content', {
		body: { revision: 1, fields: { show_adult: { value: false } } }
	});
	net.on('GET /filters/fixtures', { body: fixtures });
	net.on('POST /filters/preview/jobs', { status: 202, body: { job: 7 } });
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
	return render(FiltersScreen);
}
const job = (a: ReturnType<typeof weakening>) =>
	net.on('GET /filters/preview/jobs/{id}', { body: finished(a) });
const applied = (a: ReturnType<typeof weakening>) =>
	net.on('POST /filters/apply', { body: { applied: true, assessment: a, version: 9 } });
const lastApply = () => JSON.parse(net.to('/filters/apply').at(-1)!.body!);

afterEach(() => vi.unstubAllGlobals());
beforeEach(() => vi.useRealTimers());

describe('a pack with nothing pending', () => {
	it('shows its version, changelog as text and corpus status, and offers no Accept', async () => {
		setup([rule()]);
		await screen.findByTestId('pack-panel-malware');
		expect(screen.getByTestId('pack-panel-malware').textContent).toContain('MW-2@2');
		expect(screen.queryByTestId('pack-pending-malware')).toBeNull();
		expect(screen.queryByTestId('pack-accept-MW-2')).toBeNull();
		const log = screen.getByTestId('pack-changelog-text-malware');
		expect(log.textContent).toContain('<b>bold</b>');
		expect(log.querySelector('b')).toBeNull();
		await waitFor(() =>
			expect(screen.getByTestId('pack-corpus-malware').textContent).toContain('All examples pass')
		);
	});
	it('says the corpus status is not known when it cannot be read', async () => {
		setup([rule()]);
		net.error('GET /filters/fixtures', 500, 'down');
		await screen.findByTestId('pack-panel-malware');
		expect(screen.getByTestId('pack-corpus-malware').textContent).toContain('Not known');
	});
	it('flags failing examples and known gaps', async () => {
		const c = corpus();
		c.packs.rules[0]!.failing = 2;
		c.packs.rules[0]!.gaps = 1;
		setup([rule()], [], c);
		await waitFor(() =>
			expect(screen.getByTestId('pack-corpus-malware').textContent).toContain('2 to look at')
		);
		expect(screen.getByTestId('pack-corpus-malware').textContent).toContain('1 known gaps');
	});
});

describe('a pending strengthening version', () => {
	it('shows the changelog as text and accepts without a password', async () => {
		setup([rule({ pending: 2, version: 2, applied: 1 })], [pend]);
		const notice = await screen.findByTestId('pack-pending-MW-2');
		expect(notice.textContent).toContain('Version 1 to 2');
		expect(notice.textContent).toContain('wider');
		expect(notice.querySelector('img')).toBeNull();
		const a = strengthening({
			pack_versions: ['MW-2@2'],
			pack_acceptance: [acceptance()]
		} as never);
		job(a);
		applied(a);
		await fireEvent.click(screen.getByTestId('pack-accept-MW-2'));
		await screen.findByTestId('pack-impact-MW-2');
		expect(screen.queryByTestId('change-password')).toBeNull();
		await fireEvent.click(screen.getByTestId('change-apply'));
		await waitFor(() => expect(net.to('/filters/apply')).toHaveLength(1));
		expect(lastApply().change).toEqual({
			op: 'accept_pack_versions',
			rules: ['MW-2'],
			versions: ['MW-2@2']
		});
		expect(lastApply().password).toBeUndefined();
	});
});

describe('a pending weakening version', () => {
	it('asks for the password and sends the previewed versions with it', async () => {
		setup([rule({ pending: 2 })], [{ ...pend, direction: 'narrows' }]);
		const a = weakening({
			pack_versions: ['MW-2@2'],
			pack_acceptance: [acceptance({ strengthens: false, direction: 'narrows' })]
		} as never);
		job(a);
		applied(a);
		await fireEvent.click(await screen.findByTestId('pack-accept-MW-2'));
		const pw = await screen.findByTestId('change-password');
		expect(net.to('/filters/apply')).toHaveLength(0);
		await fireEvent.input(pw, { target: { value: 'secret' } });
		await fireEvent.click(screen.getByTestId('change-apply'));
		await waitFor(() => expect(net.to('/filters/apply')).toHaveLength(1));
		expect(lastApply().change.versions).toEqual(['MW-2@2']);
		expect(lastApply().password).toBe('secret');
	});
	it('says plainly when a change is in doubt', async () => {
		setup([rule({ pending: 2 })], [{ ...pend, direction: null }]);
		job(
			weakening({
				pack_versions: ['MW-2@2'],
				pack_acceptance: [acceptance({ in_doubt: true, strengthens: false, direction: null })]
			} as never)
		);
		await fireEvent.click(await screen.findByTestId('pack-accept-MW-2'));
		await screen.findByTestId('change-password');
		expect((await screen.findByTestId('pack-in-doubt')).textContent).toBe(
			"We can't confirm this change is safe, so it needs your password."
		);
	});
	it('previews again when the server says it changed since the preview', async () => {
		setup([rule({ pending: 2 })], [pend]);
		const a = weakening({ pack_versions: ['MW-2@2'], pack_acceptance: [acceptance()] } as never);
		job(a);
		net.error('POST /filters/apply', 409, 'changed_since_preview');
		await fireEvent.click(await screen.findByTestId('pack-accept-MW-2'));
		await fireEvent.input(await screen.findByTestId('change-password'), {
			target: { value: 'secret' }
		});
		await fireEvent.click(screen.getByTestId('change-apply'));
		await waitFor(() => expect(net.to('/filters/preview/jobs')).toHaveLength(2));
		expect(net.to('/filters/apply')).toHaveLength(1);
	});
});

describe('overrides', () => {
	const ov = (over: Record<string, unknown> = {}) => ({
		action: 'drop',
		base: 1,
		changed: false,
		paused: false,
		unless: 'name:"x"',
		unreadable: false,
		...over
	});
	it('shows yours beside the base', async () => {
		setup([rule({ pack_override: ov() })]);
		const vs = await screen.findByTestId('pack-vs-MW-2');
		expect(vs.textContent).toContain('Hide');
		expect(vs.textContent).toContain('Drop');
		expect(vs.textContent).toContain('name:"x"');
		expect(screen.queryByTestId('pack-confirm-MW-2')).toBeNull();
	});
	it('flags a base that changed', async () => {
		setup([rule({ base_changed: true, pack_override: ov({ changed: true }) })]);
		expect((await screen.findByTestId('pack-changed-MW-2')).textContent).toContain('changed');
		expect(screen.getByTestId('pack-confirm-MW-2')).toBeTruthy();
	});
	it('a paused override says the rule applies as built, and Confirm again sends the base', async () => {
		setup([rule({ base_changed: true, pack_override: ov({ changed: true, paused: true }) })]);
		expect((await screen.findByTestId('pack-paused-MW-2')).textContent).toContain('as built');
		const a = weakening({ pack_base: 2 });
		job(a);
		applied(a);
		await fireEvent.click(screen.getByTestId('pack-confirm-MW-2'));
		await fireEvent.input(await screen.findByTestId('change-password'), {
			target: { value: 'pw' }
		});
		await fireEvent.click(screen.getByTestId('change-apply'));
		await waitFor(() => expect(net.to('/filters/apply')).toHaveLength(1));
		expect(lastApply().change).toEqual({ op: 'confirm_pack_override', rule: 'MW-2', base: 2 });
	});
	it('saves an override from the editor; the server decides the password', async () => {
		setup([rule()]);
		job(strengthening({ pack_base: 1 }));
		applied(strengthening({ pack_base: 1 }));
		const edit = await screen.findByTestId('pack-edit-MW-2');
		edit.closest('details')!.open = true;
		await fireEvent.change(screen.getByTestId('pack-action-MW-2'), { target: { value: 'drop' } });
		await fireEvent.input(screen.getByTestId('pack-unless-MW-2'), {
			target: { value: 'name:"y"' }
		});
		await fireEvent.click(screen.getByTestId('pack-save-MW-2'));
		await screen.findByTestId('change-apply');
		expect(screen.queryByTestId('change-password')).toBeNull();
		await fireEvent.click(screen.getByTestId('change-apply'));
		await waitFor(() => expect(net.to('/filters/apply')).toHaveLength(1));
		expect(lastApply().change).toEqual({
			op: 'set_pack_override',
			rule: 'MW-2',
			action: 'drop',
			unless: 'name:"y"',
			base: 1
		});
	});
	it('an unreadable override says to set it again', async () => {
		setup([rule({ pack_override: ov({ unreadable: true, action: null, base: null }) })]);
		await waitFor(() =>
			expect(screen.getByTestId('pack-override-MW-2').textContent).toContain("can't be read")
		);
		expect(screen.queryByTestId('pack-vs-MW-2')).toBeNull();
	});
});

describe('review fixes', () => {
	it('offers no Quarantine in the override editor, but still names it when it applies', async () => {
		setup([rule({ action: 'quarantine', base_action: 'quarantine' })]);
		const select = (await screen.findByTestId('pack-action-MW-2')) as HTMLSelectElement;
		expect([...select.options].map((o) => o.textContent)).toEqual([
			'Flag',
			'Demote',
			'Hide',
			'Drop'
		]);
		expect(screen.getByTestId('pack-rule-MW-2').textContent).toContain('Quarantine');
	});
	it('keeps an unsaved edit across a reload that changes nothing stored', async () => {
		setup([rule()]);
		const unless = (await screen.findByTestId('pack-unless-MW-2')) as HTMLInputElement;
		await fireEvent.input(unless, { target: { value: 'name:"draft"' } });
		const a = strengthening();
		job(a);
		applied(a);
		await fireEvent.click(screen.getByTestId('pack-rule-switch-MW-2'));
		await fireEvent.click(await screen.findByTestId('change-apply'));
		// The overview is read again after the apply.
		await waitFor(() =>
			expect(net.to('/filters').filter((r) => r.path === '/filters')).toHaveLength(2)
		);
		expect((screen.getByTestId('pack-unless-MW-2') as HTMLInputElement).value).toBe('name:"draft"');
	});
	it('shows Not known when the pack has no corpus rows, and a note when the corpus fails', async () => {
		const c = corpus({ passes: false });
		setup([rule()], [], { ...c, packs: { ...c.packs, rules: [] } });
		await waitFor(() =>
			expect(screen.getByTestId('pack-corpus-malware').textContent).toContain('Not known')
		);
		expect(screen.getByTestId('pack-corpus-malware').textContent).not.toContain(
			'All examples pass'
		);
		await screen.findByTestId('pack-corpus-global');
	});
});
