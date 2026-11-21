// The page frame (X-page-frame, §10.2): every destination has exactly one `h1`, and the `h1` starts
// at the same left edge on every destination at each width. Settings is the one place where the
// edge differs by design: from 1024 px its sections sit right of the section list, so its `h1`s
// share an edge with each other (and with every other width's gutter below 1024).
// `PAGE_FRAME_TABLE=1` prints the measurements as a Markdown table (the decisions file's numbers).
// Waits are conditions with a 10 s deadline; no fixed sleeps. (Named `.routes.test.ts` so the
// `core` project picks it up.)
import type { Page } from '@playwright/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import { discoverFixture } from '../src/lib/features/insights/stages/fixtures';
import type { components } from '../src/lib/api/schema';
import type { OpenApiMock } from './mock/openapi-mock';
import { liveLogs, logLine } from './logs.stream';
import { freezeClock } from './snapshot';
import { ROUTES } from './support/routes';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });
const WIDTHS = [390, 1024, 1440] as const;

interface Destination {
	name: string;
	path: string;
	settings?: true;
	/** A selector that exists only once the screen has loaded (its own heading or root). */
	ready?: string;
	mock?: (mock: OpenApiMock) => void;
	/** Started before the page loads (a live stream). */
	before?: (page: Page) => Promise<unknown>;
}

const settingsMock =
	(extra: (on: (id: string, f: unknown) => unknown) => void) => (mock: OpenApiMock) => {
		new SettingsBackend(
			{ sections: [] } as unknown as components['schemas']['SettingsIndex'],
			[]
		).attach(mock as unknown as MockLike);
		extra(mock.on.bind(mock) as (id: string, f: unknown) => unknown);
	};

// The app's destinations: the routes of `support/routes.ts` behind the top bar, and the Settings
// screens that had no `h1`. The sign-in, setup and welcome pages have their own bare frame.
const BARE = new Set(['login', 'setup', 'welcome', 'insights-stage']);
const DESTINATIONS: Destination[] = [
	...ROUTES.filter((r) => !BARE.has(r.name)).map((r): Destination => ({
		name: r.name,
		path: r.path,
		settings: r.path.startsWith('/settings') ? true : undefined,
		mock: r.mock
	})),
	{
		name: 'insights-stage',
		path: '/insights/discover?range=30d',
		mock: (m) => m.on('getInsightsDiscover', discoverFixture())
	},
	// An unknown Insights stage: the unavailable page, on the manuscript frame.
	{ name: 'unavailable', path: '/insights/harvest' },
	{
		name: 'logs',
		path: '/logs',
		before: liveLogs,
		mock: (m) => {
			m.on('getLogHistory', {
				lines: [logLine(2), logLine(1)],
				more: false,
				next_cursor: null,
				unreadable: 0
			});
			m.on('getLogCatalog', { status: 403, body: { error: { code: 'forbidden', message: 'no' } } });
		}
	},
	{
		name: 'settings-keyboard',
		path: '/settings/keyboard',
		settings: true,
		ready: '[data-testid="keyboard-screen"]',
		mock: settingsMock((on) =>
			on('getSettings', {
				section: 'preferences',
				revision: 3,
				restart_required: [],
				fields: {
					shortcuts: {
						value: true,
						env: null,
						read_only: false,
						restart_required: false,
						source: 'db'
					},
					shortcut_keys: {
						value: {},
						env: null,
						read_only: false,
						restart_required: false,
						source: 'db'
					}
				}
			})
		)
	},
	{
		name: 'settings-overview',
		path: '/settings',
		settings: true,
		mock: settingsMock(() => {})
	},
	{
		name: 'settings-companions',
		path: '/settings/companions',
		settings: true,
		ready: '#companions-heading',
		mock: settingsMock((on) => on('listCompanions', { companions: [] }))
	},
	{
		name: 'settings-outputs',
		path: '/settings/outputs',
		settings: true,
		ready: '#outputs-torznab',
		mock: settingsMock((on) => {
			on('listKeys', { keys: [] });
			on('listJobs', { jobs: [] });
		})
	}
];

interface Measure {
	h1: number;
	left: number;
	right: number;
	count: number;
	inFrame: number;
	busy: number;
	overflow: boolean;
}

async function measure(page: Page): Promise<Measure> {
	return page.evaluate(() => {
		const heads = [...document.querySelectorAll('main h1')];
		const frame = document.querySelector('main [data-page-frame]');
		const inner = frame?.firstElementChild ?? null;
		const box = (el: Element | null) => el?.getBoundingClientRect();
		return {
			count: heads.length,
			inFrame: document.querySelectorAll('main [data-page-frame] h1').length,
			busy: document.querySelectorAll('main [role="status"][aria-busy="true"]').length,
			overflow: document.documentElement.scrollWidth > document.documentElement.clientWidth,
			h1: box(heads[0])?.left ?? -1,
			left: box(inner)?.left ?? -1,
			right: box(inner)?.right ?? -1
		};
	});
}

const rows: string[] = [];
test.afterAll(() => {
	if (process.env.PAGE_FRAME_TABLE && rows.length) console.log(rows.join('\n'));
});

for (const width of WIDTHS) {
	test.describe(`at ${width} px`, () => {
		test.use({ viewport: { width, height: 900 } });
		const edges = new Map<string, number>();

		for (const d of DESTINATIONS) {
			test(`${d.name}: one h1, on the shared edge`, async ({ page, go, mock }) => {
				await freezeClock(page);
				d.mock?.(mock);
				await d.before?.(page);
				await go(d.path);
				if (d.ready) await expect(page.locator(d.ready)).toBeAttached();
				// The static boot page has an h1 too, and a lazy screen shows a placeholder h1 at the
				// shell's edge while it loads: wait for the app's own frame, one h1 inside it, and no
				// loading status, so the screen's own heading is what gets measured.
				let m: Measure | undefined;
				await expect
					.poll(
						async () => (
							(m = await measure(page)),
							m.count === 1 && m.inFrame === 1 && m.busy === 0 && m.left >= 0
						),
						{ message: 'one h1 inside the page frame, loaded' }
					)
					.toBe(true);
				const got = m!;
				rows.push(`| ${d.name} | ${width} | ${got.h1} | ${got.left} | ${got.right} |`);
				edges.set(d.name, got.h1);
				if (width === 390) expect(got.overflow, 'no horizontal scroll at 390 px').toBe(false);

				// The gutter: 16 px below `large` (640), 24 px from it.
				const gutter = width >= 640 ? 24 : 16;
				if (d.settings && width >= 1024) expect(got.h1).toBe(312);
				else expect(got.h1).toBe(gutter);
			});
		}

		test('every Settings screen shares one h1 edge', () => {
			const settings = DESTINATIONS.filter((d) => d.settings).map((d) => edges.get(d.name));
			// Runs after the tests above in this file (serial within a describe at one worker); skip
			// when a filter ran only some of them.
			test.skip(
				settings.some((e) => e === undefined),
				'filtered run'
			);
			expect(new Set(settings).size).toBe(1);
		});
	});
}
