// Sources (SRC-15a, §18.8): the shapes of `GET /sources` / `PATCH /sources/{id}`, the grouping by
// kind, and the row's state. Nothing here judges a consent: the server's gate (§18.7) decides what
// an enable needs, and this screen never sends an enable the row says still needs setup.
import { ApiError } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { m } from '$lib/paraglide/messages.js';

type Schemas = components['schemas'];
export type Source = Schemas['Source'];
export type SourceKind = Schemas['SourceKind'];
export type SourcePatch = Schemas['SourcePatch'];
export type Priority = NonNullable<SourcePatch['priority']>;

export const KINDS: readonly SourceKind[] = [
	'discovery',
	'importer',
	'peer_finder',
	'enrichment',
	'network',
	'output'
];
export const PRIORITIES: readonly Priority[] = ['low', 'normal', 'high', 'max'];

/** A source sits under its first listed kind; one with no kind known here goes last, by name. */
export function groupOf(s: Source): SourceKind | null {
	return s.kinds.find((k) => KINDS.includes(k)) ?? null;
}

export interface Group {
	kind: SourceKind | null;
	sources: Source[];
}

/** Groups in `KINDS` order, rows by name; empty groups are left out. */
export function group(sources: readonly Source[]): Group[] {
	const out: Group[] = [...KINDS, null].map((kind) => ({ kind, sources: [] }));
	for (const s of sources) out.find((g) => g.kind === groupOf(s))!.sources.push(s);
	for (const g of out) g.sources.sort((a, b) => a.name.localeCompare(b.name));
	return out.filter((g) => g.sources.length > 0);
}

export type Status =
	'purge_pending' | 'purged' | 'needs_setup' | 'error' | 'running' | 'waiting' | 'paused' | 'off';

/**
 * What a user must still do before the server lets this source on; null when enabling is a
 * toggle. `needs` is the server's own answer (null once the class's condition holds, so a source
 * acknowledged earlier toggles back on). `password_required` asks every time: for Restricted and
 * Overlay that's a setup step of its own, the SRC-15b prompt. For Account it isn't: every Account
 * enable already goes through the Account dialog, which asks for whatever the class needs, and
 * while this build's gate refuses Account the dialog shows that refusal. So a disabled Account
 * source whose credentials are stored shows no "needs your password" note
 * (X-sources-restructure, from X-consent-gate-lift r2, which makes `password_required` true for
 * Account). The server's gate still judges every PATCH.
 */
export function setupNeed(s: Source): 'consent' | 'endpoints' | 'credentials' | 'password' | null {
	if (s.enabled) return null;
	if (s.needs === 'credentials') return 'credentials';
	if (s.needs === 'endpoints_ack') return 'endpoints';
	if (s.needs === 'consent') return 'consent';
	return s.password_required && s.consent !== 'account' ? 'password' : null;
}

export function statusOf(s: Source): Status {
	if (s.purge_pending) return 'purge_pending';
	if (s.state === 'purged') return 'purged';
	if (s.enabled) {
		if (s.health.last_error !== null && !s.health.running) return 'error';
		return s.health.running ? 'running' : 'waiting';
	}
	if (setupNeed(s) !== null) return 'needs_setup';
	return s.state === 'paused' ? 'paused' : 'off';
}

export const STATUS_VARIANT: Record<Status, 'success' | 'info' | 'warning' | 'error' | 'neutral'> =
	{
		purge_pending: 'warning',
		purged: 'neutral',
		needs_setup: 'warning',
		error: 'error',
		running: 'success',
		waiting: 'info',
		paused: 'neutral',
		off: 'neutral'
	};

export const statusText = (st: Status): string =>
	({
		purge_pending: m.sources_status_purge_pending,
		purged: m.sources_status_purged,
		needs_setup: m.sources_status_needs_setup,
		error: m.sources_status_error,
		running: m.sources_status_running,
		waiting: m.sources_status_waiting,
		paused: m.sources_status_paused,
		off: m.sources_status_off
	})[st]();

export const kindText = (k: SourceKind | null): string =>
	k === null
		? m.sources_kind_other()
		: {
				discovery: m.sources_kind_discovery,
				importer: m.sources_kind_importer,
				peer_finder: m.sources_kind_peer_finder,
				enrichment: m.sources_kind_enrichment,
				network: m.sources_kind_network,
				output: m.sources_kind_output
			}[k]();

export const priorityText = (p: Priority): string =>
	({
		low: m.sources_priority_low,
		normal: m.sources_priority_normal,
		high: m.sources_priority_high,
		max: m.sources_priority_max
	})[p]();

export const setupText = (n: NonNullable<ReturnType<typeof setupNeed>>): string =>
	({
		consent: m.sources_setup_consent,
		endpoints: m.sources_setup_endpoints,
		credentials: m.sources_setup_credentials,
		password: m.sources_setup_password
	})[n]();

/** Plain messages for a refused change; anything else says what the server said. */
export function refusalText(e: unknown): string {
	if (!(e instanceof ApiError) || e.status === 0) return m.sources_error_network();
	switch (e.code) {
		case 'consent_required':
		case 'credentials_required':
		case 'confirm_required':
			return m.sources_error_needs_setup();
		case 'purge_running':
			return m.sources_error_purge_running();
		default:
			return m.sources_error_code({ code: e.code });
	}
}

export const riskText = (r: Source['risk']): string =>
	({ low: m.sources_risk_low, medium: m.sources_risk_medium, high: m.sources_risk_high })[r]();

export const consentText = (c: Source['consent']): string | null =>
	c === 'none'
		? null
		: {
				external: m.sources_consent_external,
				account: m.sources_consent_account,
				restricted: m.sources_consent_restricted,
				overlay: m.sources_consent_overlay
			}[c]();

/**
 * Whether a source matches the filter box: its name or one of its kinds, ignoring case. An empty
 * query matches everything.
 */
export function matches(s: Source, query: string): boolean {
	const q = query.trim().toLocaleLowerCase();
	if (!q) return true;
	const kinds = s.kinds.filter((k) => KINDS.includes(k)).map((k) => kindText(k));
	if (groupOf(s) === null) kinds.push(kindText(null));
	return [s.name, ...kinds].some((t) => t.toLocaleLowerCase().includes(q));
}

/**
 * Whether a source needs the person's attention: it failed, it waits for a restart, a purge
 * didn't finish, or it's on and waits for a companion. A source that is simply off, or still needs setup before it can be
 * turned on, doesn't.
 */
export function needsAttention(s: Source): boolean {
	return (
		statusOf(s) === 'error' ||
		s.restart_required ||
		s.purge_pending ||
		(s.enabled && s.companion_need !== null)
	);
}

/** A group's one-line summary: how many of its sources are on, and how many need attention. */
export function summary(sources: readonly Source[]): {
	on: number;
	total: number;
	attention: number;
} {
	return {
		on: sources.filter((s) => s.enabled).length,
		total: sources.length,
		attention: sources.filter(needsAttention).length
	};
}

/** The consent classes in the legend's order, `none` shown as Open. */
export const CONSENT_CLASSES: readonly Source['consent'][] = [
	'none',
	'external',
	'account',
	'restricted',
	'overlay'
];

export const consentName = (c: Source['consent']): string =>
	consentText(c) ?? m.sources_consent_open();

export const consentHelp = (c: Source['consent']): string =>
	({
		none: m.sources_legend_open,
		external: m.sources_legend_external,
		account: m.sources_legend_account,
		restricted: m.sources_legend_restricted,
		overlay: m.sources_legend_overlay
	})[c]();
