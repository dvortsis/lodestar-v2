import { describe, expect, it } from 'vitest';
import { indexView } from '../form/schema';
import { spec } from '../form/testing/values';
import { matchesScreen, screenFields, screens } from './screens';

describe('the implemented Settings destinations', () => {
	it('combines crawler and transport in one screen, without exposing other sections', () => {
		const fields = [spec('crawler.paused'), spec('transport.utp'), spec('logging.kinds')];
		const view = {
			...indexView({ sections: [] }),
			fieldsOf: (names: string[]) => fields.filter((field) => names.includes(field.section))
		};
		expect(screenFields(screens[0]!, view).map((field) => field.key)).toEqual([
			'crawler.paused',
			'transport.utp'
		]);
	});
	it('keeps unsafe changes locked without mutating the shared schema', () => {
		const fields = [spec('network.http_bind', { danger: 'disruptive' }), spec('network.dht_port')];
		const view = { ...indexView({ sections: [] }), fieldsOf: () => fields };
		expect(screenFields(screens[1]!, view).every((field) => field.readOnly)).toBe(true);
		expect(fields.every((field) => !field.readOnly)).toBe(true);
		const dangerous = [
			spec('storage.purge', { danger: 'destructive' }),
			spec('storage.private', { danger: 'privacy' }),
			spec('storage.lock', { danger: 'security' })
		];
		expect(
			screenFields(screens[2]!, { ...view, fieldsOf: () => dangerous }).every(
				(field) => field.readOnly
			)
		).toBe(true);
	});
	it('matches translated section labels without case or accent sensitivity', () => {
		const screen = { ...screens[0]!, label: () => 'Réseau 网络', purpose: () => 'VPN connections' };
		expect(matchesScreen(screen, 'RESE vpn')).toBe(true);
		expect(matchesScreen(screen, '网络')).toBe(true);
		expect(matchesScreen(screen, 'logging')).toBe(false);
	});
});
