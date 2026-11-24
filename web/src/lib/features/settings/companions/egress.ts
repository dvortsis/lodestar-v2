// A companion's route in words, and a route change's answers (X-cmp-egress-ui). Loaded with the
// route editor only, so the list's first view carries none of it.
import { ApiError } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';
import type { components } from '$lib/api/schema';
import { failureWords } from './logic';

export type EgressSetting = components['schemas']['EgressSetting'];
export type EgressChanged = components['schemas']['CompanionEgressChanged'];

/** A route in words. Null means the saved row can't be read: the route stays down, never direct. */
export function routeText(s: EgressSetting | null): string {
	if (s === null) return m.settings_companions_route_unreadable();
	const r = (route: string) => m.settings_companions_route({ route });
	switch (s.route) {
		case 'direct':
			return r(m.settings_companions_route_direct());
		case 'socks5':
			return r(m.settings_companions_route_socks5({ proxy: s.proxy ?? '' }).trimEnd());
		case 'interface':
			return r(m.settings_companions_route_interface());
		default:
			return r(m.settings_companions_route_bind_address());
	}
}

/** Why a saved route didn't restart the companion now (`not_restarted`), in words. */
const NOT_RESTARTED: Record<string, () => string> = {
	not_wired: m.settings_companions_nr_not_wired,
	busy: m.settings_companions_nr_busy,
	restart_failed: m.settings_companions_nr_restart_failed
};
export function notRestartedWords(code: string | null): string {
	const f = code && Object.hasOwn(NOT_RESTARTED, code) ? NOT_RESTARTED[code] : undefined;
	return (f ?? m.settings_companions_nr_other)();
}

/** A route change's refusals (`PUT …/egress`): each its own sentence. The field is named, never the value. */
const EGRESS: Record<string, () => string> = {
	unknown_companion: m.settings_companions_eg_unknown_companion,
	takes_no_route: m.settings_companions_eg_takes_no_route,
	not_installed: m.settings_companions_eg_not_installed,
	busy: m.settings_companions_eg_busy,
	cant_follow_route: m.settings_companions_eg_cant_follow_route,
	route_unavailable: m.settings_companions_eg_route_unavailable,
	not_wired: m.settings_companions_eg_not_wired,
	bad_body: m.settings_companions_eg_bad_body
};
export function egressWords(e: unknown): string {
	if (!(e instanceof ApiError)) return failureWords(e);
	if (e.code === 'invalid_egress')
		return (
			e.field === 'egress.proxy'
				? m.settings_companions_eg_invalid_proxy
				: e.field === 'egress.dns'
					? m.settings_companions_eg_invalid_dns
					: m.settings_companions_eg_invalid_other
		)();
	const f = Object.hasOwn(EGRESS, e.code) ? EGRESS[e.code] : undefined;
	return f ? f() : failureWords(e);
}
/** The refusals the password dialog answers itself (wrong, missing, too many tries, signed out). */
export const passwordRefusal = (e: unknown) =>
	e instanceof ApiError &&
	(e.status === 401 || e.code === 'confirm_required' || e.code === 'rate_limited');
