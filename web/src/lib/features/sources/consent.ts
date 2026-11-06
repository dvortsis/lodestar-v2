// The approved consent texts (SRC-4 "Draft consent texts" with owner §2.49's Account change), filled
// from the API's row. The wording lives in en.json; `consent.test.ts` compares what the dialogs
// show with a copy of the approved section. This file only chooses which message and fills it in.
// It never decides that consent is satisfied: the server's answer is the truth.
import { ApiError } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';
import type { Source } from './sources';

export type ConsentClass = Exclude<Source['consent'], 'none'>;

export interface ConsentBlocks {
	title: string;
	paragraphs: string[];
	/** Shown after the first paragraph; each may carry `**bold**`. */
	bullets: string[];
	closing: string | null;
	confirm: string;
}

/** The distinct host names of the source's endpoints (a bare host is kept as it is). */
export function hostsOf(endpoints: readonly string[]): string {
	const seen = new Set<string>();
	for (const e of endpoints) {
		let host = e;
		try {
			host = new URL(e).host;
		} catch {
			// not a URL: show it as the server gave it
		}
		seen.add(host);
	}
	return [...seen].join(', ');
}

type Route = NonNullable<Source['egress']['setting']>['route'];

/** An unset route is the default, a direct connection. */
export const routeText = (route: Route | undefined): string =>
	route === undefined || route === 'direct'
		? m.sources_cx_route_direct()
		: route === 'socks5'
			? m.sources_cx_route_socks5()
			: m.sources_cx_route_other();

/** The overlay network's name: the first network that isn't BitTorrent, else the source's name. */
export const networkOf = (s: Source): string => s.networks.find((n) => n !== 'bt') ?? s.name;

export function blocksFor(s: Source, cls: ConsentClass): ConsentBlocks {
	const name = s.name;
	const hosts = hostsOf(s.endpoints);
	switch (cls) {
		case 'external':
			return {
				title: m.sources_cx_external_title({ name }),
				paragraphs: [
					m.sources_cx_external_p1({ hosts }),
					m.sources_cx_external_p2({ egress: routeText(s.egress.setting?.route) })
				],
				bullets: [],
				closing: null,
				confirm: m.sources_cx_external_confirm({ name })
			};
		case 'account':
			return {
				title: m.sources_cx_account_title({ name }),
				paragraphs: [m.sources_cx_account_p1({ name, hosts }), m.sources_cx_account_p2()],
				bullets: [],
				closing: null,
				confirm: m.sources_cx_account_confirm()
			};
		case 'restricted':
			return {
				title: m.sources_cx_restricted_title({ name }),
				paragraphs: [m.sources_cx_restricted_p1({ name })],
				bullets: [
					m.sources_cx_restricted_b1(),
					m.sources_cx_restricted_b2(),
					m.sources_cx_restricted_b3()
				],
				closing: m.sources_cx_restricted_p2(),
				confirm: m.sources_cx_restricted_confirm({ name })
			};
		case 'overlay':
			return {
				title: m.sources_cx_overlay_title({ name }),
				paragraphs: [m.sources_cx_overlay_p1({ name, network: networkOf(s) })],
				bullets: [m.sources_cx_overlay_b1(), m.sources_cx_overlay_b2(), m.sources_cx_overlay_b3()],
				closing: m.sources_cx_overlay_p2(),
				confirm: m.sources_cx_overlay_confirm({ name })
			};
	}
}

/** Splits `**bold**` runs: odd parts are bold. */
export const richParts = (text: string): { text: string; bold: boolean }[] =>
	text.split('**').map((t, i) => ({ text: t, bold: i % 2 === 1 }));

/** A restricted or overlay source is enabled with the admin password (Q13). */
export const needsPassword = (cls: ConsentClass): boolean =>
	cls === 'restricted' || cls === 'overlay';

/**
 * A refusal of an enable, in §10.14 form (what happened, then what to do). The X-consent-gate-052
 * refusal is shown plainly with the server's own reason; so is any other refusal of `enabled`.
 */
export function enableRefusal(e: unknown): { text: string; stale: boolean; password: boolean } {
	if (!(e instanceof ApiError) || e.status === 0)
		return { text: m.sources_error_network(), stale: false, password: false };
	if (e.status === 401)
		return { text: m.sources_cx_wrong_password(), stale: false, password: true };
	if (e.code === 'consent_required')
		return { text: m.sources_cx_stale(), stale: true, password: false };
	if (e.status === 400 && e.field === 'enabled')
		return { text: m.sources_cx_gate({ reason: e.message }), stale: false, password: false };
	if (e.code === 'confirm_required')
		return { text: m.sources_cx_password_needed(), stale: false, password: true };
	if (e.code === 'purge_running')
		return { text: m.sources_error_purge_running(), stale: false, password: false };
	return {
		text: e.message
			? m.sources_cx_refused({ reason: e.message })
			: m.sources_error_code({ code: e.code }),
		stale: false,
		password: false
	};
}
