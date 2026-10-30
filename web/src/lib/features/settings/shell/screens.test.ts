import { describe, expect, it, vi } from 'vitest';
import { indexView } from '../form/schema';
import { spec } from '../form/testing/values';
import { collect, matchesScreen, screenFields, screens, type Screen } from './screens';

const byId = (id: string) => screens.find((screen) => screen.id === id)!;
const fake = (id: string, group: Screen['group']): Screen => ({
	id,
	group,
	sections: [id],
	title: () => id,
	purpose: () => `${id} purpose`
});

describe('the screen registry', () => {
	it('finds the generated screens through their screen.ts, in navigation order', () => {
		expect(screens.map((screen) => screen.id)).toEqual([
			'keyboard',
			'crawler',
			'network',
			'storage',
			'search',
			'general'
		]);
		expect(screens.filter((screen) => screen.sections).every((screen) => screen.icon)).toBe(true);
		// Keyboard is hand-built (S-keys): the route loads its body, which renders its own heading.
		expect(byId('keyboard').load).toBeTypeOf('function');
	});
	it('orders by group, then §10.10.2, then title, and takes every export shape once', () => {
		const logging = fake('logging', 'system');
		const got = collect({
			'/b/screen.ts': { screens: [fake('zeta', 'system'), fake('alpha', 'system')] },
			'/a/screen.ts': { screen: logging, default: logging },
			'/c/screen.ts': { default: fake('look', 'preferences') },
			'/d/screen.ts': { screens: [fake('about', 'system'), fake('general', 'system')] }
		});
		expect(got.map((screen) => screen.id)).toEqual([
			'look',
			'general',
			'logging',
			'about',
			'alpha',
			'zeta'
		]);
	});
	it('leaves out invalid and repeated screens and names them in development', () => {
		const error = vi.spyOn(console, 'error').mockImplementation(() => {});
		const got = collect({
			'/a/screen.ts': { screen: fake('crawler', 'crawling') },
			'/b/screen.ts': { screen: fake('crawler', 'system') },
			'/c/screen.ts': { screens: [{ ...fake('x', 'system'), group: 'nowhere' }] },
			'/d/screen.ts': { screen: { id: 'empty', group: 'system', title: () => 'Empty' } },
			'/e/screen.ts': {}
		});
		expect(got.map((screen) => `${screen.id}:${screen.group}`)).toEqual(['crawler:crawling']);
		expect(error).toHaveBeenCalledTimes(3);
		error.mockRestore();
	});
});

describe('the implemented Settings destinations', () => {
	it('combines crawler and transport in one screen, without exposing other sections', () => {
		const fields = [spec('crawler.paused'), spec('transport.utp'), spec('logging.kinds')];
		const view = {
			...indexView({ sections: [] }),
			fieldsOf: (names: string[]) => fields.filter((field) => names.includes(field.section))
		};
		expect(screenFields(byId('crawler'), view).map((field) => field.key)).toEqual([
			'crawler.paused',
			'transport.utp'
		]);
	});
	it('keeps unsafe changes locked without mutating the shared schema', () => {
		const fields = [spec('network.http_bind', { danger: 'disruptive' }), spec('network.dht_port')];
		const view = { ...indexView({ sections: [] }), fieldsOf: () => fields };
		expect(screenFields(byId('network'), view).every((field) => field.readOnly)).toBe(true);
		expect(fields.every((field) => !field.readOnly)).toBe(true);
		const dangerous = [
			spec('storage.purge', { danger: 'destructive' }),
			spec('storage.private', { danger: 'privacy' }),
			spec('storage.lock', { danger: 'security' })
		];
		expect(
			screenFields(byId('storage'), { ...view, fieldsOf: () => dangerous }).every(
				(field) => field.readOnly
			)
		).toBe(true);
	});
	it('matches translated section labels without case or accent sensitivity', () => {
		const screen = { ...screens[0]!, title: () => 'Réseau 网络', purpose: () => 'VPN connections' };
		expect(matchesScreen(screen, 'RESE vpn')).toBe(true);
		expect(matchesScreen(screen, '网络')).toBe(true);
		expect(matchesScreen(screen, 'logging')).toBe(false);
	});
});
