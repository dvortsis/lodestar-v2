// Blocklist subscriptions (S-blocklists, §20.13, owner §2.9): the shapes of `GET /filters/
// subscriptions`, the change builders for the filters flows, and what a refused change says.
// Nothing here judges a change: the preview/apply flow and the server decide what needs the password.
import { ApiError } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { m } from '$lib/paraglide/messages.js';

type Schemas = components['schemas'];
export type Subscriptions = Schemas['Subscriptions'];
export type CatalogList = Schemas['CatalogList'];
export type ConnectedList = Schemas['ConnectedList'];
export type Strictness = Schemas['Strictness'];
export type StrictnessList = Schemas['StrictnessList'];
export type Change = Schemas['Change'];

export const connect = (l: CatalogList): Change => ({
	op: 'subscribe',
	url: l.url,
	kind: l.kind,
	format: l.format,
	name: l.name
});
export const pause = (id: number, enabled: boolean): Change => ({
	op: 'pause_subscription',
	id,
	enabled
});
export const remove = (id: number): Change => ({ op: 'unsubscribe', id });
/** Accepting names no version here: the flow sends back the preview's `held_version`. */
export const settle = (id: number, accept: boolean): Change => ({ op: 'resolve_held', id, accept });

/** The level word, as the server reports it; an unknown word reads as the strictest. */
export function levelName(level: string): string {
	return (
		{
			none: m.blocklists_level_none,
			notice: m.blocklists_level_notice,
			strong: m.blocklists_level_strong,
			severe: m.blocklists_level_severe
		}[level] ?? m.blocklists_level_severe
	)();
}

/** Plain messages for a refused change; anything else says what the server said. */
export function refusalText(e: unknown): string {
	if (!(e instanceof ApiError)) return m.blocklists_error_network();
	if (e.status === 0) return m.blocklists_error_network();
	switch (e.code) {
		case 'list_url_refused':
			return m.blocklists_error_url_refused();
		case 'bad_list_url':
			return m.blocklists_error_url_bad();
		case 'bad_list_name':
			return m.blocklists_error_name();
		case 'unknown_list_kind':
		case 'unknown_list_format':
		case 'bad_list_action':
			return m.blocklists_error_list_type();
		case 'held_needs_version':
		case 'changed_since_preview':
			return m.blocklists_error_changed();
		default:
			return e.message.trim() || m.blocklists_error_code({ code: e.code });
	}
}
