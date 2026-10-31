// The names Settings → Logging shows for the catalog's ids: data kinds, levels and components.
import { m } from '$lib/paraglide/messages.js';
import type { DataKind, LogLevel } from './rules';

const KINDS: Record<DataKind, () => string> = {
	hash: m.settings_logging_kind_hash,
	name: m.settings_logging_kind_name,
	path: m.settings_logging_kind_path,
	url: m.settings_logging_kind_url,
	query: m.settings_logging_kind_query,
	client: m.settings_logging_kind_client,
	peer: m.settings_logging_kind_peer,
	size: m.settings_logging_kind_size,
	peer_activity: m.settings_logging_kind_peer_activity,
	filter_changes: m.settings_logging_kind_filter_changes
};

/** A data kind's name ("torrent names"); an unknown one by its id. */
export const kindLabel = (k: string) => KINDS[k as DataKind]?.() ?? k;

const LEVELS: Record<LogLevel, () => string> = {
	error: m.settings_logging_level_error,
	warn: m.settings_logging_level_warn,
	info: m.settings_logging_level_info,
	debug: m.settings_logging_level_debug,
	trace: m.settings_logging_level_trace
};
export const levelLabel = (l: string) => LEVELS[l as LogLevel]?.() ?? l;

export { timeLeft } from './state';
