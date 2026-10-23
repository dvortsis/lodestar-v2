import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { buildRegistry, type AttentionItem, type SettingsScreen } from './registry';
import { buildBadges } from './badges';
import { collectAttention, sortItems } from './attention';
import { checkBody, goldenView, screen, settingsApi, systemInfo } from './testing';
import { screens as mine } from '../screens/screen';
import { loadSettingMessages } from '../form/messages';

const registry = (extra: SettingsScreen[] = []) => buildRegistry(goldenView(), [...mine, ...extra]);
const signal = new AbortController().signal;
const item = (
	id: string,
	screenId: string,
	severity: AttentionItem['severity']
): AttentionItem => ({
	id,
	screen: screenId,
	severity,
	text: () => id,
	action: { label: () => 'go', href: '/x' }
});
const ids = (items: AttentionItem[]) => items.map((i) => i.id);

beforeEach(async () => {
	vi.spyOn(console, 'error').mockImplementation(() => {});
	await loadSettingMessages([]);
});
afterEach(() => vi.unstubAllGlobals());

describe('sortItems', () => {
	it('orders problem, then restart, then notice, then by the screens’ order', () => {
		const r = registry();
		const sorted = sortItems(
			[
				item('notice-crawler', 'crawler', 'notice'),
				item('restart-general', 'general', 'restart'),
				item('problem-storage', 'storage', 'problem'),
				item('problem-network', 'network', 'problem'),
				item('restart-crawler', 'crawler', 'restart'),
				item('problem-unknown', 'zzz', 'problem')
			],
			r
		);
		expect(ids(sorted)).toEqual([
			'problem-network',
			'problem-storage',
			'problem-unknown',
			'restart-crawler',
			'restart-general',
			'notice-crawler'
		]);
	});

	it('keeps the order given within one severity and screen', () => {
		const r = registry();
		expect(
			ids(sortItems([item('b', 'crawler', 'notice'), item('a', 'crawler', 'notice')], r))
		).toEqual(['b', 'a']);
	});
});

describe('collectAttention', () => {
	it('lists a waiting restart, with Restart now when it is available', async () => {
		settingsApi({ 'GET /network/check': () => ({ status: 200, body: checkBody() }) });
		const a = await collectAttention({
			system: systemInfo({ restart_required: ['network.dht_port', 'crawler.encryption'] }),
			registry: registry(),
			signal
		});
		expect(ids(a.items)).toEqual(['restart']);
		expect(a.items[0]!.text()).toBe('2 changes wait for a restart');
		expect(a.items[0]!.screen).toBe('network');
		expect(a.items[0]!.action.label()).toBe('Restart now');
		expect(a.items[0]!.action.run).toBeTypeOf('function');
		expect(a.partial).toBe(false);
	});

	it('says how to restart when it cannot, and uses the singular for one change', async () => {
		settingsApi({ 'GET /network/check': () => ({ status: 200, body: checkBody() }) });
		const a = await collectAttention({
			system: systemInfo({
				restart_required: ['crawler.encryption'],
				restart: {
					available: false,
					estimated_seconds: 20,
					hint: 'Restart the container',
					method: 'manual'
				}
			}),
			registry: registry(),
			signal
		});
		expect(a.items[0]!.text()).toBe('1 change waits for a restart');
		expect(a.items[0]!.action.label()).toBe('How to restart');
		expect(a.items[0]!.action.href).toBe('#settings-restart-banner');
	});

	it('flags an unreachable DHT port with the configured port, and links to Network', async () => {
		settingsApi({
			'GET /network/check': () => ({
				status: 200,
				body: checkBody({ reachable: false, reason: 'no_unsolicited_queries' })
			}),
			'GET /settings/network': () => ({
				status: 200,
				body: {
					section: 'network',
					revision: 1,
					restart_required: [],
					fields: {
						dht_port: {
							value: 7000,
							source: 'db',
							env: null,
							read_only: false,
							restart_required: true
						}
					}
				}
			})
		});
		const a = await collectAttention({ system: systemInfo(), registry: registry(), signal });
		expect(a.items).toHaveLength(1);
		expect(a.items[0]!.text()).toBe("Port 7000 isn't reachable from the internet");
		expect(a.items[0]!.severity).toBe('problem');
		expect(a.items[0]!.action.href).toBe('/settings/network');
		expect(a.items[0]!.action.label()).toBe('Open Network & VPN');
	});

	it('says nothing about a port when reachability is unknown', async () => {
		settingsApi({
			'GET /network/check': () => ({ status: 200, body: checkBody({ reachable: null }) })
		});
		const a = await collectAttention({ system: systemInfo(), registry: registry(), signal });
		expect(a.items).toEqual([]);
		expect(a.partial).toBe(false);
	});

	it('leaves the row out and marks the result partial when the network check fails', async () => {
		settingsApi({
			'GET /network/check': () => ({
				status: 500,
				body: { error: { code: 'internal', message: 'x' } }
			})
		});
		const a = await collectAttention({
			system: systemInfo({ restart_required: ['crawler.encryption'] }),
			registry: registry(),
			signal
		});
		expect(ids(a.items)).toEqual(['restart']);
		expect(a.partial).toBe(true);
	});

	it('shows an address awaiting confirmation as a problem and a reverted one as a notice', async () => {
		settingsApi({ 'GET /network/check': () => ({ status: 200, body: checkBody() }) });
		const change = {
			deadline: 2_000,
			kind: 'http_bind' as const,
			previous_url: 'http://192.168.1.5:8090/',
			state: 'awaiting_confirmation' as const,
			stored: true,
			url: 'http://192.168.1.5:9090/'
		};
		const waiting = await collectAttention({
			system: systemInfo({ address_change: change }),
			registry: registry(),
			signal
		});
		expect(waiting.items[0]).toMatchObject({
			id: 'address-confirm',
			severity: 'problem',
			screen: 'network'
		});
		expect(waiting.items[0]!.text()).toContain('http://192.168.1.5:8090/');
		const reverted = await collectAttention({
			system: systemInfo({ address_change: { ...change, state: 'reverted', deadline: null } }),
			registry: registry(),
			signal
		});
		expect(reverted.items[0]!.text()).toBe('Lodestar went back to http://192.168.1.5:8090/');
		expect(reverted.items[0]).toMatchObject({ severity: 'notice' });
	});

	it('adds every screen’s rows, in order, and survives a provider that fails', async () => {
		settingsApi({ 'GET /network/check': () => ({ status: 200, body: checkBody() }) });
		const good = screen('updates', {
			group: 'system',
			attention: async () => [item('update-ready', 'updates', 'notice')]
		});
		const broken = screen('logging', {
			group: 'system',
			attention: async () => {
				throw new Error('boom');
			}
		});
		const slow = screen('insights', {
			group: 'connections',
			attention: async () => [item('integration-failing', 'insights', 'problem')]
		});
		const a = await collectAttention({
			system: systemInfo({ restart_required: ['crawler.encryption'] }),
			registry: registry([good, broken, slow]),
			signal
		});
		expect(ids(a.items)).toEqual(['integration-failing', 'restart', 'update-ready']);
	});
});

describe('buildBadges', () => {
	const r = registry();
	it('counts problems per screen, with the count in the text', () => {
		const badges = buildBadges(
			[
				item('a', 'network', 'problem'),
				item('b', 'network', 'problem'),
				item('c', 'storage', 'problem')
			],
			[],
			r
		);
		expect(badges.network).toEqual({ text: '2 problems', label: '2 problems', kind: 'problem' });
		expect(badges.storage!.text).toBe('1 problem');
	});

	it('marks screens with restart-pending fields, and lets a problem win', () => {
		const badges = buildBadges(
			[item('a', 'network', 'problem')],
			['network.dht_port', 'crawler.encryption'],
			r
		);
		expect(badges.network!.kind).toBe('problem');
		expect(badges.crawler).toEqual({
			text: 'Restart needed',
			label: 'Restart needed',
			kind: 'restart'
		});
	});

	it('ignores notices and restart keys no screen owns', () => {
		expect(buildBadges([item('n', 'crawler', 'notice')], ['nothing.here'], r)).toEqual({});
	});
});
