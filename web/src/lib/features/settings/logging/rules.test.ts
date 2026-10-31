// Settings → Logging's rules against the server's (`settings/logging.rs`, `logging/state.rs`): which
// events a verbosity turns on, Reset to preset, and the patches the privacy changes send.
import { describe, expect, it } from 'vitest';
import {
	deadline,
	eventOn,
	kindPatch,
	offPatch,
	presetOn,
	presetPatch,
	endOf,
	toggleEvent,
	identifyingOn,
	type Catalog,
	type LogEvent,
	type LogKind
} from './rules';
import { timeLeft } from './state';

const ev = (over: Partial<LogEvent>): LogEvent => ({
	id: 'fetch.failed',
	component: 'fetch',
	level: 'info',
	default_on: true,
	current: true,
	required: false,
	opt_in: null,
	kinds: [],
	label_key: 'logs.event.fetch.failed.label',
	help_key: 'logs.event.fetch.failed.help',
	rate_per_hour: 0,
	...over
});
const kind = (k: LogKind['kind'], over: Partial<LogKind> = {}): LogKind => ({
	kind: k,
	identifying: !['client', 'size', 'peer_activity', 'filter_changes'].includes(k),
	opt_in: k === 'peer_activity' || k === 'filter_changes',
	default_on: k === 'client' || k === 'size',
	until: 0,
	outputs: { console: false, file: false, live: false, event_log: false },
	...over
});
const NOW = 1_000_000;

describe('events', () => {
	it('turns on events at or above the level, default-off ones only from Detailed', () => {
		expect(presetOn(ev({ level: 'info' }), 'normal', {})).toBe(true);
		expect(presetOn(ev({ level: 'debug' }), 'normal', {})).toBe(false);
		expect(presetOn(ev({ default_on: false }), 'normal', {})).toBe(false);
		expect(presetOn(ev({ default_on: false }), 'detailed', {})).toBe(true);
		expect(presetOn(ev({ level: 'warn' }), 'errors', {})).toBe(false);
	});
	it('uses a component level over the verbosity', () => {
		expect(presetOn(ev({ level: 'debug' }), 'normal', { fetch: 'debug' })).toBe(true);
		expect(presetOn(ev({ level: 'info' }), 'everything', { fetch: 'error' })).toBe(false);
	});
	it('keeps required events on and opt-in events with their opt-in', () => {
		const required = ev({ required: true, level: 'trace' });
		expect(eventOn(required, 'errors', {}, { [required.id]: false })).toBe(true);
		const optIn = ev({ opt_in: 'filter_changes', current: false });
		expect(presetOn(optIn, 'everything', {})).toBe(false);
		expect(eventOn(optIn, 'everything', {}, { [optIn.id]: true })).toBe(false);
		expect(toggleEvent(optIn, true, 'normal', {}, {})).toEqual({});
		expect(toggleEvent(required, false, 'normal', {}, {})).toEqual({});
	});
	it('stores only overrides that differ from the preset (Reset to preset is {})', () => {
		const e = ev({});
		const off = toggleEvent(e, false, 'normal', {}, {});
		expect(off).toEqual({ 'fetch.failed': false });
		expect(eventOn(e, 'normal', {}, off)).toBe(false);
		expect(toggleEvent(e, true, 'normal', {}, off)).toEqual({});
	});
});

describe('privacy patches', () => {
	const v = { data: 'private', data_until: 0, kinds: {} };
	it('sets a timed deadline on the server clock', () => {
		expect(deadline('hour', NOW + 0.7)).toBe(NOW + 3600);
		expect(deadline('day', NOW)).toBe(NOW + 86_400);
		expect(deadline('off', NOW)).toBe(-1);
	});
	it('turns an identifying kind on only with its deadline, and off with 0 or nothing', () => {
		expect(kindPatch(v, kind('name'), true, NOW + 3600, NOW)).toEqual({
			kinds: { name: NOW + 3600 }
		});
		expect(kindPatch({ ...v, kinds: { name: NOW + 5 } }, kind('name'), false, 0, NOW)).toEqual({
			kinds: {}
		});
		// The Hashes preset has hashes on: turning them off needs an explicit 0.
		const hashes = { data: 'hashes', data_until: NOW + 100, kinds: {} };
		expect(kindPatch(hashes, kind('hash'), false, 0, NOW)).toEqual({ kinds: { hash: 0 } });
	});
	it('switches non-identifying kinds for good', () => {
		expect(kindPatch(v, kind('client'), false, 0, NOW)).toEqual({ kinds: { client: 0 } });
		expect(kindPatch({ ...v, kinds: { client: 0 } }, kind('client'), true, 0, NOW)).toEqual({
			kinds: {}
		});
	});
	it('a preset never touches the opt-ins and clears per-kind switches', () => {
		const p = presetPatch('full', NOW + 3600);
		expect(p).toEqual({ data: 'full', data_until: NOW + 3600, kinds: {} });
		expect(Object.keys(p)).not.toContain('peer_activity_until');
		expect(Object.keys(p)).not.toContain('change_details_until');
		expect(presetPatch('private', NOW + 3600).data_until).toBe(0);
		// Client addresses turned off survive a preset; identifying switches don't.
		const c = { kinds: [kind('name'), kind('client'), kind('peer_activity')] } as Catalog;
		expect(presetPatch('hashes', NOW + 60, { kinds: { client: 0, name: -1 } }, c).kinds).toEqual({
			client: 0
		});
	});
	it('Turn off now keeps only switches of kinds that identify no one', () => {
		const c = { kinds: [kind('name'), kind('client'), kind('peer_activity')] } as Catalog;
		expect(offPatch({ data: 'full', data_until: -1, kinds: { name: -1, client: 0 } }, c)).toEqual({
			data: 'private',
			data_until: 0,
			kinds: { client: 0 }
		});
	});
});

describe('banners', () => {
	it('shows when logging of what the server has on stops', () => {
		const c = {
			kinds: [
				kind('hash', { until: NOW + 7200 }),
				kind('name', { until: NOW + 600 }),
				kind('path', { until: NOW - 1 }),
				kind('peer_activity', { until: NOW + 60 })
			]
		} as Catalog;
		const on = identifyingOn(c, NOW);
		expect(on.map((k) => k.kind)).toEqual(['hash', 'name']);
		expect(endOf(on)).toBe(NOW + 7200);
		expect(endOf([{ until: NOW + 5 }, { until: -1 }])).toBe(-1);
		expect(endOf([])).toBe(0);
	});
	it('says nothing once a deadline has passed (the server turns it off)', () => {
		expect(timeLeft(NOW - 1, NOW)).toBeNull();
		expect(timeLeft(0, NOW)).toBeNull();
		expect(timeLeft(NOW + 600, NOW)).toContain('10 min');
		expect(timeLeft(-1, NOW)).toContain('until you turn it off');
	});
});
