// The approved consent texts (SRC-4 "Draft consent texts" with owner §2.49's Account change), filled
// from the API's row. The wording lives in en.json; `dialogs.test.ts` compares what the dialogs
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

/**
 * Stands for the address list inside a filled message. The dialog swaps it for the full addresses
 * the acknowledgement covers (`Source.endpoints`, exactly as the server compares them), set as text
 * so an address can't be read as markup or bold marks.
 */
export const ADDRESSES_MARK = '\uE000';

type Route = NonNullable<Source['egress']['setting']>['route'];

/** An unset route is the default, a direct connection. */
export const routeText = (route: Route | undefined): string =>
	route === undefined || route === 'direct'
		? m.sources_cx_route_direct()
		: route === 'socks5'
			? m.sources_cx_route_socks5()
			: m.sources_cx_route_other();

/** The overlay network's name: the first network that isn't plain BitTorrent, else the source's name. */
export const networkOf = (s: Source): string => {
	const id = s.networks.find((n) => n !== 'bt');
	return id === undefined ? s.name : (NETWORK_NAMES[id] ?? id);
};

/** The API's network ids as the networks are called (proper names, the same in every language). */
const NETWORK_NAMES: Record<string, string> = {
	bt_i2p: 'I2P',
	ed2k: 'eD2k',
	ipfs: 'IPFS',
	usenet: 'Usenet'
};

export function blocksFor(s: Source, cls: ConsentClass): ConsentBlocks {
	const name = s.name;
	const hosts = ADDRESSES_MARK;
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

/**
 * Splits `**bold**` runs (odd parts are bold), and each run at the address mark: the pieces between
 * marks are text, and an address list belongs at each gap. The marks are split on the template
 * before any address is put in, so an address with `**` in it stays literal.
 */
export const richParts = (text: string): { pieces: string[]; bold: boolean }[] =>
	text.split('**').map((t, i) => ({ pieces: t.split(ADDRESSES_MARK), bold: i % 2 === 1 }));

/** A restricted or overlay source is enabled with the admin password (Q13). */
export const needsPassword = (cls: ConsentClass): boolean =>
	cls === 'restricted' || cls === 'overlay';

/**
 * A refusal of an enable, in §10.14 form (what happened, then what to do). The X-consent-gate-052
 * refusal is shown plainly with the server's own reason; so is any other refusal of `enabled`.
 */
export function enableRefusal(e: unknown): { text: string; stale: boolean; password: boolean } {
	const out = (text: string, extra: { stale?: boolean; password?: boolean } = {}) => ({
		text,
		stale: extra.stale ?? false,
		password: extra.password ?? false
	});
	if (!(e instanceof ApiError) || e.status === 0) return out(m.sources_error_network());
	if (e.code === 'bad_password') return out(m.sources_cx_wrong_password(), { password: true });
	if (e.code === 'confirm_required') return out(m.sources_cx_password_needed(), { password: true });
	if (e.code === 'consent_required') return out(m.sources_cx_stale(), { stale: true });
	if (e.code === 'purge_running') return out(m.sources_error_purge_running());
	// The server's own sentence says what happened and what to do (the consent gate's, "purge it
	// again", ...); it never repeats a secret.
	return out(
		e.message ? m.sources_cx_gate({ reason: e.message }) : m.sources_error_code({ code: e.code })
	);
}
