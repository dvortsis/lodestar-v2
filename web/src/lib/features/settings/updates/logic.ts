// Settings → Updates' pure rules (§10.10.14, §12.2). Free of the DOM so each is unit-tested alone.
// The screen shows only what the server says: these functions read a `GET /updates` body and the
// `updates` settings values, and decide nothing the server has not already said.
import type { components } from '$lib/api/schema';

export type Status = components['schemas']['UpdatesStatus'];
export type Release = components['schemas']['ReleaseInfo'];
export type HistoryEntry = components['schemas']['HistoryEntry'];
export type Mode = 'off' | 'notify' | 'auto' | 'image';
export type Level = 'fixes' | 'features' | 'all';

export const MODES: readonly Mode[] = ['off', 'notify', 'auto', 'image'];
export const LEVELS: readonly Level[] = ['fixes', 'features', 'all'];
export const PAUSE_DAYS = [1, 7, 30] as const;

/** The engine states in which an install is under way (the status shows its steps). */
export const STEPS = [
	'downloading',
	'verifying',
	'installing',
	'restarting',
	'checking_health'
] as const;
export type Step = (typeof STEPS)[number];
export const isStep = (s: string | null | undefined): s is Step => STEPS.includes(s as Step);

export const isMode = (v: unknown): v is Mode => MODES.includes(v as Mode);
export const isLevel = (v: unknown): v is Level => LEVELS.includes(v as Level);

/** "Always install security fixes" per mode: on under automatic, off under Check and notify. */
export function securityDefault(mode: Mode): boolean {
	return mode === 'auto';
}

/** The toggle is hidden under Off and Container image updates (§10.10.14). */
export const securityVisible = (mode: Mode): boolean => mode === 'notify' || mode === 'auto';

/**
 * The toggle's value is the server's effective one (`policy.security_fixes`). Until the server has
 * said, it is the mode's default, never a guess beyond that.
 */
export function securityValue(status: Status | undefined, mode: Mode): boolean {
	return status ? status.policy.security_fixes : securityDefault(mode);
}

/** The newest release that is not skipped by the server's own reasons. */
export const newest = (status: Status): Release | undefined => status.releases[0];

/** The automatic options are disabled with this reason beside them; null when they work. */
export function cannotAuto(status: Status | undefined): string | null {
	if (status === undefined) return null;
	if (status.unavailable) return status.unavailable;
	return status.capability.can_install ? null : (status.capability.reason ?? 'unknown');
}

/** An https URL only: release notes links come from signed data, but a link is still checked. */
export function safeHttps(url: string | null | undefined): string | null {
	try {
		const u = new URL(url ?? '');
		return u.protocol === 'https:' && !u.username && !u.password ? u.href : null;
	} catch {
		return null;
	}
}

/** The badges a release shows: its level, then Security and Manual steps. */
export function badges(r: Release): string[] {
	const out: string[] = [];
	if (r.level === 'fix' || r.level === 'feature' || r.level === 'major') out.push(r.level);
	if (r.security) out.push('security');
	if (r.manual_steps) out.push('manual');
	return out;
}

/** The notice codes the screen words itself, in the order it lists them. */
export const NOTICE_ORDER = [
	'revoked_key',
	'trust_damaged',
	'clock_behind',
	'clock_ahead',
	'updater_unreadable',
	'check_not_saved',
	'pending_unwritable',
	'bad_overlay',
	'manifest_lapsed',
	'channel_unavailable'
] as const;
export type NoticeCode = (typeof NOTICE_ORDER)[number];

/** The notices to show: known codes in order, then `held_in_process`. `rolled_back` is the status's. */
export function noticesOf(status: Status): (NoticeCode | 'held_in_process')[] {
	const out: (NoticeCode | 'held_in_process')[] = NOTICE_ORDER.filter((c) =>
		status.notices.includes(c)
	);
	const held = status.policy.held_in_process && !out.includes('updater_unreadable');
	return held ? [...out, 'held_in_process'] : out;
}
