import type { components } from '../../src/lib/api/schema';
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
	{
		name: 'alerts',
		path: '/alerts',
		heading: 'Alerts',
		mock: (m) => m.on('listSavedSearches', [SAVED_SEARCH])
	},
	{
		name: 'alert-detail',
		path: '/alerts/1',
		heading: 'Lantern Keeper releases',
		mock: (m) => {
			m.on('getSavedSearch', SAVED_SEARCH);
			m.on('listAlerts', { items: [], next_cursor: null });
			m.on('markAlertsRead', { ok: true });
		}
	},
	{ name: 'insights', path: '/insights' },
	{ name: 'insights-stage', path: '/insights/harvest' },
	{ name: 'compare', path: '/compare?h=aaaa,bbbb' },
	{
		name: 'login',
		path: '/login',
		heading: 'Sign in',
		mock: (m) => m.on('getAuthStatus', { logged_in: false, setup_required: false })
	},
	{
		name: 'setup',
		path: '/setup',
		heading: 'Set up Lodestar',
		mock: (m) => m.on('getAuthStatus', { logged_in: false, setup_required: true })
	},
	{ name: 'welcome', path: '/welcome', key: true },
	{ name: 'settings', path: '/settings/general', key: true },
	{ name: 'torrent', path: `/t/${'0123456789abcdef'.repeat(2).padEnd(40, '0')}`, key: true }
];

// What the Alerts routes' smoke runs load: one saved search with nothing unread.
const SAVED_SEARCH: components['schemas']['SavedSearchView'] = {
	alert: true,
	alerts: 0,
	ast: { op: 'all' },
	created_at: 1_700_000_000,
	id: 1,
	last_alert_at: null,
	name: 'Lantern Keeper releases',
	paused: false,
	q: '',
	rss: false,
	sort: null,
	unread: 0,
	updated_at: 1_700_000_000
};
