import type { OpenApiMock } from '../mock/openapi-mock';

// Every route the app has today, with sample params for the dynamic ones. A new route slice adds
// its entry here (smoke tests then cover it in every project); `coverage.test.ts` flags gaps.
export interface RouteCase {
	/** Short name used in test titles and snapshot file names. */
	name: string;
	path: string;
	/** The route's `<h1>`, when the page has one. */
	heading?: string;
	/**
	 * A key screen: snapshotted at every breakpoint × Riverstone light/dark and in all 46 other
	 * themes. Other routes get axe and a render check everywhere but no baseline (keeps the
	 * committed PNGs, and so the repo, small; see README "Snapshot policy").
	 */
	key?: true;
	/** Density changes this screen, so it is also snapshotted in Compact. */
	densityAware?: true;
	/** Mocks for the requests the route makes on load. Any unmocked call fails the test. */
	mock?: (mock: OpenApiMock) => void;
}

export const ROUTES: RouteCase[] = [
	{ name: 'search', path: '/search', heading: 'Search', key: true, densityAware: true },
	{ name: 'discover', path: '/discover' },
	{ name: 'alerts', path: '/alerts' },
	{ name: 'alert-detail', path: '/alerts/1' },
	{ name: 'insights', path: '/insights' },
	{ name: 'insights-stage', path: '/insights/harvest' },
	{ name: 'compare', path: '/compare?h=aaaa,bbbb' },
	{ name: 'login', path: '/login' },
	{ name: 'setup', path: '/setup' },
	{ name: 'welcome', path: '/welcome', key: true },
	{ name: 'settings', path: '/settings/general', key: true },
	{ name: 'torrent', path: `/t/${'0123456789abcdef'.repeat(2).padEnd(40, '0')}`, key: true }
];
