import { HASH, mockTorrent } from '../detail.fixtures';
import type { OpenApiMock } from '../mock/openapi-mock';
import { discoverResponse, showAdultSettings } from '../../src/lib/features/discover/testing';
import { crawlerFixture, overviewFixture } from '../../src/lib/features/insights/overview/fixtures';

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
	{
		name: 'search',
		path: '/search',
		heading: 'Search',
		key: true,
		densityAware: true,
		// The bare page parses nothing; an interaction with a query in the bar would.
		mock: (m) => m.on('parseQuery', { ast: { op: 'all' }, canonical: '' })
	},
	{
		name: 'discover',
		path: '/discover',
		heading: 'Discover',
		mock: (m) => {
			m.on('getDiscover', discoverResponse());
			m.on('getSettings', showAdultSettings(false));
		}
	},
	{
		name: 'alerts',
		path: '/alerts',
		heading: 'Alerts',
		mock: (m) =>
			m.on('listSavedSearches', [
				{
					alert: true,
					alerts: 0,
					ast: { op: 'all' as const },
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
				}
			])
	},
	{
		name: 'alert-detail',
		path: '/alerts/1',
		heading: 'Lantern Keeper releases',
		mock: (m) => {
			m.on('getSavedSearch', {
				alert: true,
				alerts: 0,
				ast: { op: 'all' as const },
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
			});
			m.on('listAlerts', { items: [], next_cursor: null });
			m.on('markAlertsRead', { ok: true });
		}
	},
	{
		name: 'insights',
		path: '/insights',
		heading: 'Insights',
		mock: (m) => {
			m.on('getInsightsOverview', overviewFixture());
			m.on('getCrawler', crawlerFixture());
		}
	},
	{ name: 'insights-stage', path: '/insights/harvest' },
	{
		name: 'compare',
		path: `/compare?h=${'a'.repeat(40)},${'b'.repeat(40)}`,
		heading: 'Compare',
		mock: (m) =>
			m.on('compareTorrents', {
				torrents: ['a', 'b'].map((c, i) => ({
					hash: c.repeat(40),
					name: `The.Lantern.Keeper.${i ? '1080p' : '2160p'}`,
					title: 'The Lantern Keeper',
					content_type: 'movie',
					size: (2 - i) * 1_000_000_000,
					files: 1,
					seeders: null,
					leechers: null,
					trackers_reporting: null,
					trackers_checked: null,
					discovered_at: 1_760_000_000,
					composition: [],
					attributes: { res: i ? '1080p' : '2160p' }
				})),
				differs: { res: true },
				scale_bytes: 2_000_000_000,
				file_diff: {
					common: { count: 0, files: [], truncated: false },
					only: [
						{ count: 0, files: [], truncated: false },
						{ count: 0, files: [], truncated: false }
					]
				}
			})
	},
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
	{
		name: 'welcome',
		path: '/welcome',
		heading: 'Setup complete',
		key: true,
		// Signed in with setup done, or the page redirects to sign-in (or setup) instead.
		mock: (m) => m.on('getAuthStatus', { logged_in: true, setup_required: false })
	},
	{ name: 'settings', path: '/settings/general', key: true },
	{
		name: 'torrent',
		path: `/t/${HASH}`,
		key: true,
		mock: (m) => mockTorrent(m)
	}
];
