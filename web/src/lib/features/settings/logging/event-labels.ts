// The event list's names (lazy, with it): components, the data kinds' short badge names, and each
// event's plain name and help. The catalog names an event's messages (`logs.event.<id>.label`);
// until they're written (the runner's follow-up), an event without one shows its stable id.
import { m } from '$lib/paraglide/messages.js';
import type { DataKind, LogEvent } from './rules';

const COMPONENTS: Record<string, () => string> = {
	system: m.settings_logging_component_system,
	dht: m.settings_logging_component_dht,
	fetch: m.settings_logging_component_fetch,
	pipeline: m.settings_logging_component_pipeline,
	search: m.settings_logging_component_search,
	api: m.settings_logging_component_api,
	sources: m.settings_logging_component_sources,
	filters: m.settings_logging_component_filters,
	integrations: m.settings_logging_component_integrations,
	workers: m.settings_logging_component_workers,
	companions: m.settings_logging_component_companions,
	auth: m.settings_logging_component_auth,
	libraries: m.settings_logging_component_libraries
};
export const COMPONENT_IDS = Object.keys(COMPONENTS);
export const componentLabel = (c: string) => COMPONENTS[c]?.() ?? c;

const SHORT: Record<DataKind, () => string> = {
	hash: m.settings_logging_short_hash,
	name: m.settings_logging_short_name,
	path: m.settings_logging_short_path,
	url: m.settings_logging_short_url,
	query: m.settings_logging_short_query,
	client: m.settings_logging_short_client,
	peer: m.settings_logging_short_peer,
	size: m.settings_logging_short_size,
	peer_activity: m.settings_logging_short_peer_activity,
	filter_changes: m.settings_logging_short_filter_changes
};
export const kindShort = (k: string) => SHORT[k as DataKind]?.() ?? k;

/** Event messages by key, once they exist; empty until then. */
const EVENTS: Record<string, () => string> = {};
const text = (key: string) => EVENTS[key.replaceAll('.', '_')]?.();

export const eventLabel = (e: Pick<LogEvent, 'id' | 'label_key'>) => text(e.label_key) ?? e.id;
export const eventHelp = (e: Pick<LogEvent, 'help_key'>) => text(e.help_key);
