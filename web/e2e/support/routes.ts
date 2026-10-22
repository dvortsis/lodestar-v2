// Every route the app has today, with sample params for the dynamic ones. A new route slice adds
// its entry here (smoke tests then cover it in every project); `coverage.test.ts` flags gaps.
export interface RouteCase {
	/** Short name used in test titles and snapshot file names. */
	name: string;
	path: string;
	/** The route's `<h1>`, when the page has one. */
	heading?: string;
}

export const ROUTES: RouteCase[] = [
	{ name: 'search', path: '/search', heading: 'Search' },
	{ name: 'discover', path: '/discover' },
	{ name: 'alerts', path: '/alerts' },
	{ name: 'alert-detail', path: '/alerts/1' },
	{ name: 'insights', path: '/insights' },
	{ name: 'insights-stage', path: '/insights/harvest' },
	{ name: 'compare', path: '/compare?h=aaaa,bbbb' },
	{ name: 'login', path: '/login' },
	{ name: 'setup', path: '/setup' },
	{ name: 'welcome', path: '/welcome' },
	{ name: 'settings', path: '/settings/general' },
	{ name: 'torrent', path: `/t/${'0123456789abcdef'.repeat(2).padEnd(40, '0')}` }
];
