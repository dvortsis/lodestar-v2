// The five stable destinations (§10.3), the overflow menu and the active-state rule. Labels are
// message calls so a language change re-renders them.
import { m } from '$lib/paraglide/messages.js';

// `resolve()` is typed by route id; our links are dynamic strings (query, density), so they are
// cast to one id. There is no base path, so it returns the path unchanged.
export type Route = '/search';

export type DestinationId = 'search' | 'discover' | 'alerts' | 'insights' | 'settings';

export interface Destination {
	id: DestinationId;
	href: string;
	label: () => string;
}

export const DESTINATIONS: readonly Destination[] = [
	{ id: 'search', href: '/search', label: () => m.shell_nav_search() },
	{ id: 'discover', href: '/discover', label: () => m.shell_nav_discover() },
	{ id: 'alerts', href: '/alerts', label: () => m.shell_nav_alerts() },
	{ id: 'insights', href: '/insights', label: () => m.shell_nav_insights() },
	{ id: 'settings', href: '/settings/general', label: () => m.shell_nav_settings() }
];

export interface MoreItem {
	id: 'import' | 'backup' | 'logs' | 'syntax' | 'about';
	href: string;
	label: () => string;
}

// Targets belong to later slices (Settings sections §10.10, the live log §12.1, the syntax guide);
// until a slice lands its page, the link shows the 404 state. `about` is Settings → About (§10.10).
export const MORE: readonly MoreItem[] = [
	{ id: 'import', href: '/settings/import', label: () => m.shell_more_import() },
	{ id: 'backup', href: '/settings/backup', label: () => m.shell_more_backup() },
	{ id: 'logs', href: '/logs', label: () => m.shell_more_logs() },
	{ id: 'syntax', href: '/help/syntax', label: () => m.shell_more_syntax() },
	{ id: 'about', href: '/settings/about', label: () => m.shell_more_about() }
];

/** Whether `pathname` is inside the destination (`/insights/harvest` is inside `/insights`). */
export function isActive(destination: Destination, pathname: string): boolean {
	const root = '/' + destination.href.split('/')[1];
	return pathname === root || pathname.startsWith(root + '/');
}

/** Density is URL state (§10.3): keep it when moving between destinations. */
export function withDensity(href: string, search: URLSearchParams): string {
	const density = search.get('density');
	if (!density) return href;
	return `${href}${href.includes('?') ? '&' : '?'}density=${encodeURIComponent(density)}`;
}
