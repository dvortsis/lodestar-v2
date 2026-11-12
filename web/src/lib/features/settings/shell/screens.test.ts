import { describe, expect, it, vi } from 'vitest';
import { indexView } from '../form/schema';
import { spec } from '../form/testing/values';
import { aboutOf, collect, matchesScreen, screenFields, screens, type Screen } from './screens';

const byId = (id: string) => screens.find((screen) => screen.id === id)!;
const fake = (id: string, group: Screen['group']): Screen => ({
	id,
	group,
	sections: [id],
	title: () => id
});

describe('the screen registry', () => {
	it('finds the generated screens through their screen.ts, in navigation order', () => {
		expect(screens.map((screen) => screen.id)).toEqual([
			'appearance',
			'keyboard',
			'language',
			'crawler',
			'network',
			'sources',
			'jobs',
			'companions',
			'storage',
			'search',
			'trackers',
			'filters',
			'blocklists',
			'integrations',
			'outputs',
			'notifications',
			'insights',
			'general',
			'updates',
			'logging',
			'security',
			'about'
		]);
		expect(
			screens.filter((screen) => screen.sections).every((screen) => screen.icon || screen.iconLoad)
		).toBe(true);
		// Appearance (S-appearance) and Keyboard (S-keys) are hand-built: the route loads their body,
		// which renders its own heading.
		expect(byId('appearance').load).toBeTypeOf('function');
		expect(byId('keyboard').load).toBeTypeOf('function');
	});
	it('orders by group, then §10.10.2, then title, and takes a screen or a list once', () => {
		const logging = fake('logging', 'system');
		const got = collect({
			'/b/screen.ts': [fake('zeta', 'system'), fake('alpha', 'system')],
			'/a/screen.ts': logging,
			'/a2/screen.ts': [logging],
			'/c/screen.ts': fake('look', 'preferences'),
			'/d/screen.ts': [fake('about', 'system'), fake('general', 'system')]
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
			'/a/screen.ts': fake('crawler', 'crawling'),
			'/b/screen.ts': fake('crawler', 'system'),
			'/c/screen.ts': [{ ...fake('x', 'system'), group: 'nowhere' }],
			'/d/screen.ts': { id: 'empty', group: 'system', title: () => 'Empty' },
			'/e/screen.ts': undefined
		});
		expect(got.map((screen) => `${screen.id}:${screen.group}`)).toEqual(['crawler:crawling']);
		// The repeat, the bad group, the screen with no body, and the module with no default export.
		expect(error).toHaveBeenCalledTimes(4);
		error.mockRestore();
	});
	it('reads every screen.ts by its default export, so none can be left out', async () => {
		const modules = import.meta.glob<Record<string, unknown>>([
			'/src/lib/features/settings/*/screen.ts',
			'/src/lib/features/*/screen.ts'
		]);
		const paths = Object.keys(modules);
		expect(paths.length).toBeGreaterThan(10);
		for (const path of paths) {
			const mod = await modules[path]!();
			expect(mod.default, `${path} needs an export default`).toBeDefined();
		}
		const listed = (await Promise.all(paths.map((p) => modules[p]!()))).flatMap((m) =>
			Array.isArray(m.default) ? m.default : [m.default]
		);
		expect(screens.length).toBe(new Set(listed.map((s) => (s as { id: string }).id)).size);
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
		const screen = { ...screens[0]!, title: () => 'Réseau 网络' };
		const about = { purpose: () => 'VPN connections' };
		expect(matchesScreen(screen, 'RESE vpn', about)).toBe(true);
		expect(matchesScreen(screen, 'RESE vpn')).toBe(false);
		expect(matchesScreen(screen, '网络')).toBe(true);
		expect(matchesScreen(screen, 'logging', about)).toBe(false);
	});
});

describe('the registry keeps long strings off the first view (X-settings-registry-slim)', () => {
	it('carries no purpose or notice on an entry; `about` loads them by screen id', async () => {
		for (const screen of screens) {
			expect(Object.keys(screen)).not.toContain('purpose');
			expect(Object.keys(screen)).not.toContain('notice');
		}
		expect((await aboutOf(byId('crawler'))).purpose?.()).toBe(
			'Adjust discovery, fetch limits and peer transport.'
		);
		expect((await aboutOf(byId('network'))).notice?.()).toMatch(/read-only/);
		expect((await aboutOf(byId('trackers'))).purpose).toBeTypeOf('function');
		expect((await aboutOf(byId('language'))).purpose).toBeTypeOf('function');
		// A screen without one, or one its file's module doesn't name, has none.
		expect(await aboutOf(byId('notifications'))).toEqual({});
		expect(await aboutOf({ ...byId('crawler'), id: 'elsewhere' })).toEqual({});
	});
	it('gives every generated screen a purpose and a lazily loaded icon or the gear', async () => {
		for (const screen of screens.filter((s) => s.sections)) {
			expect((await aboutOf(screen)).purpose, screen.id).toBeTypeOf('function');
			if (screen.iconLoad) expect((await screen.iconLoad()).default, screen.id).toBeTruthy();
		}
		expect(byId('general').icon).toBeTruthy();
	});
	it('rejects when the about chunk cannot load', async () => {
		const broken = { ...byId('crawler'), about: () => Promise.reject(new Error('chunk')) };
		await expect(aboutOf(broken)).rejects.toThrow('chunk');
	});
});
