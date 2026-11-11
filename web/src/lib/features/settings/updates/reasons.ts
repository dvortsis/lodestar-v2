// Plain sentences for the stable codes the update engine sends (§10.10.14): the screen never prints
// a code, so each lookup returns words or nothing, and the caller picks a generic fallback.
import { m } from '$lib/paraglide/messages.js';

/** A plain sentence per stable install-failure code; never the code's own text, never a URL. */
export const failedWords: Record<string, () => string> = {
	download_mismatch: m.settings_updates_failed_download_mismatch,
	download_too_large: m.settings_updates_failed_download_too_large,
	download_io: m.settings_updates_failed_download_io,
	no_artifact: m.settings_updates_failed_no_artifact,
	redirect_refused: m.settings_updates_failed_redirect_refused,
	stage_failed: m.settings_updates_failed_stage_failed,
	no_space: m.settings_updates_failed_no_space,
	bad_overlay: m.settings_updates_failed_bad_overlay,
	timeout: m.settings_updates_failed_timeout,
	network: m.settings_updates_failed_network,
	blocked: m.settings_updates_failed_blocked,
	http_status: m.settings_updates_failed_http_status,
	too_large: m.settings_updates_failed_too_large,
	bin_unavailable: m.settings_updates_failed_bin_unavailable,
	rebuild_running: m.settings_updates_failed_rebuild_running,
	trust_damaged: m.settings_updates_failed_trust_damaged,
	not_current: m.settings_updates_failed_not_current
};

/** Why the last check failed; every signature or key problem reads the same to a user. */
const checkWhy: Record<string, () => string> = {
	no_release_source: m.settings_updates_check_why_no_release_source,
	network: m.settings_updates_check_why_network,
	timeout: m.settings_updates_check_why_timeout,
	redirect_refused: m.settings_updates_check_why_redirect_refused,
	blocked: m.settings_updates_check_why_blocked,
	http_status: m.settings_updates_check_why_http_status,
	too_large: m.settings_updates_check_why_too_large
};
const SIGNATURE_CODES = [
	'bad_key',
	'bad_signature_file',
	'legacy_signature',
	'wrong_key',
	'bad_signature',
	'bad_global_signature',
	'no_release_key',
	'no_nightly_key',
	'no_freshness_key',
	'unknown_key',
	'revoked_key',
	'not_recovery_signed',
	'key_reused'
];
export const checkFailedWords = (code: string | null): string =>
	(code && SIGNATURE_CODES.includes(code)
		? m.settings_updates_check_why_signature()
		: code && checkWhy[code]?.()) || m.settings_updates_check_why_other();

/** A history entry's or rollback's reason in words, or null for a code this screen doesn't know. */
const historyWhy: Record<string, () => string> = {
	failed: m.settings_updates_history_reason_failed,
	selftest: m.settings_updates_history_reason_selftest,
	manual: m.settings_updates_history_reason_manual,
	user: m.settings_updates_history_reason_user
};
export const historyReasonWords = (code: string | null): string | null =>
	(code && (historyWhy[code] ?? failedWords[code])?.()) || null;

/** The no-engine banner: a sentence per reason the server gives, the generic one for any other. */
const noEngine: Record<string, () => string> = {
	fixture: m.settings_updates_unavailable_fixture,
	network: m.settings_updates_unavailable_network,
	windows_helper: m.settings_updates_unavailable_windows_helper
};
export const noEngineWords = (why: string | null | undefined): string =>
	(why && noEngine[why]?.()) || m.settings_updates_unavailable();
