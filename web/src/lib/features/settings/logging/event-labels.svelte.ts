// The event list's names (lazy, with it): components, the data kinds' short badge names, and each
// event's plain name and help.
import { m } from '$lib/paraglide/messages.js';
import { CHUNKS, type EventNames } from './event-names';
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

// Each event's plain name and help are an on-demand chunk per component (`event-names/<component>.ts`,
// listed in `event-names.ts`), loaded when an event of that component is first shown; until then,
// and for an event the client doesn't know, the name is the stable id.
type Shown = Pick<LogEvent, 'id' | 'component'>;
let names = $state.raw<Record<string, EventNames>>({});
const loading: Record<string, Promise<void> | undefined> = Object.create(null);
/** Loads one component's event names once; the labels below update when they arrive. */
export function loadEventNames(component: string): Promise<void> {
	const chunk = Object.hasOwn(CHUNKS, component) ? CHUNKS[component] : undefined;
	if (!chunk) return Promise.resolve();
	let p = loading[component];
	if (!p) {
		p = chunk().then(
			(mod) => void (names = { ...names, [component]: mod.NAMES }),
			() => void (loading[component] = undefined)
		);
		loading[component] = p;
	}
	return p;
}

function entry(e: Shown) {
	void loadEventNames(e.component);
	return names[e.component]?.[e.id];
}
export const eventLabel = (e: Shown) => entry(e)?.[0]() ?? e.id;
export const eventHelp = (e: Shown) => entry(e)?.[1]();
