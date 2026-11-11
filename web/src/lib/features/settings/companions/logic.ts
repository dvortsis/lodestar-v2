// Companions' words and shapes. Every failure shows a plain sentence per stable code; an unknown
// code gets a generic sentence that never prints the code, the server's text or a request body.
import { ApiError } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';
import type { components } from '$lib/api/schema';

export type Companion = components['schemas']['Companion'];
export type InstallOffer = components['schemas']['InstallOffer'];
export type RemovalPlan = components['schemas']['RemovalPlan'];
export type Detected = components['schemas']['DetectedCompanion'];
export type Template = components['schemas']['CompanionTemplate'];
export type TemplateRequest = components['schemas']['TemplateRequest'];

/** What an install or an update is refused for, in words (the row's `refusal`, or a 409's code). */
const CODES: Record<string, () => string> = {
	native_in_container: m.settings_companions_code_native_in_container,
	unsupported_platform: m.settings_companions_code_unsupported_platform,
	awaiting_pin: m.settings_companions_code_awaiting_pin,
	no_build: m.settings_companions_code_awaiting_pin,
	guided_only: m.settings_companions_code_guided_only,
	already_current: m.settings_companions_code_already_current,
	confirmation_mismatch: m.settings_companions_code_confirmation_mismatch,
	not_wired: m.settings_companions_code_not_wired,
	busy: m.settings_companions_code_busy,
	not_installed: m.settings_companions_code_not_installed,
	companions_unavailable: m.settings_companions_code_unavailable,
	unknown_companion: m.settings_companions_code_unknown_companion,
	bad_password: m.settings_companions_code_bad_password,
	confirm_required: m.settings_companions_code_confirm_required,
	rate_limited: m.settings_companions_code_rate_limited,
	no_credential: m.settings_companions_code_no_credential,
	bad_value: m.settings_companions_code_bad_value,
	no_target: m.settings_companions_code_no_target,
	handover_later: m.settings_companions_code_handover_later,
	installed: m.settings_companions_code_installed,
	already_added: m.settings_companions_code_already_added,
	generated_pending: m.settings_companions_code_generated_pending,
	not_detected: m.settings_companions_code_not_detected,
	address_refused: m.settings_companions_code_address_refused,
	bad_template_input: m.settings_companions_code_bad_template_input,
	no_template: m.settings_companions_code_no_template,
	companion_unreachable: m.settings_companions_code_unreachable,
	companion_unauthorized: m.settings_companions_code_unauthorized,
	companion_protocol: m.settings_companions_code_protocol,
	network: m.settings_companions_code_network
};

/** A plain sentence for a refusal code; a code nobody here knows gets the generic one. */
export function wordsFor(code: string | null | undefined): string {
	const f = code && Object.hasOwn(CODES, code) ? CODES[code] : undefined;
	return (f ?? m.settings_companions_code_other)();
}

/** A plain sentence for a failed request. Never the error's own text. */
export function failureWords(e: unknown): string {
	return wordsFor(e instanceof ApiError ? e.code : null);
}

export const isHealth = (h: unknown): h is 'unknown' | 'healthy' | 'unhealthy' =>
	h === 'unknown' || h === 'healthy' || h === 'unhealthy';

/** Companions whose traffic is Prowlarr's or rides with it (FlareSolverr). */
export const prowlarrFamily = (id: string) => id === 'prowlarr' || id === 'flaresolverr';

/** Where the route notes apply: the companions that take a route (Q18). */
export const hasRoute = (id: string) => id === 'qbittorrent' || prowlarrFamily(id);

/**
 * A download size in IEC units, as `src/lib/format`'s `formatBytes` prints it (`512 B`, `38.2 MiB`,
 * `4.37 GiB`). Kept here: importing the shared formatter splits its chunk and adds ≈ 120 B to the
 * routes held at their §10.1 caps (docs/decisions/2026-11-11-CMP-9a.md).
 */
export function bytesText(n: number, locale: string): string {
	const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
	let i = 0;
	let v = Math.max(0, n);
	while (v >= 1024 && i < units.length - 1) {
		v /= 1024;
		i++;
	}
	const digits = i === 0 ? 0 : i >= 3 ? 2 : 1;
	const num = new Intl.NumberFormat(locale, {
		minimumFractionDigits: digits,
		maximumFractionDigits: digits
	}).format(v);
	return `${num}\u00a0${units[i]}`;
}
