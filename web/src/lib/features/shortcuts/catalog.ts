// The actions Settings can remap: the server's `DEFAULT_SHORTCUTS` (preferences.rs), in §10.16's
// order. The Settings screen lists these, not `keys.list()`: on that page the search and detail
// commands are not registered.
import { DEFAULT_KEYS } from '$lib/keys/defaults';
import { m } from '$lib/paraglide/messages.js';

export const ACTION_IDS = [
	'search.focus',
	'results.next',
	'results.previous',
	'detail.open_magnet',
	'detail.copy_magnet',
	'detail.send',
	'results.select',
	'help.shortcuts'
] as const;

export type ActionId = (typeof ACTION_IDS)[number];

/** The default key of a remappable action. */
export const defaultKey = (id: ActionId): string => DEFAULT_KEYS[id][0];

const TITLES: Record<ActionId, () => string> = {
	'search.focus': () => m.shortcuts_action_search_focus(),
	'results.next': () => m.shortcuts_action_results_next(),
	'results.previous': () => m.shortcuts_action_results_previous(),
	'detail.open_magnet': () => m.shortcuts_action_detail_open_magnet(),
	'detail.copy_magnet': () => m.shortcuts_action_detail_copy_magnet(),
	'detail.send': () => m.shortcuts_action_detail_send(),
	'results.select': () => m.shortcuts_action_results_select(),
	'help.shortcuts': () => m.shortcuts_action_help_shortcuts()
};

export const actionTitle = (id: ActionId): string => TITLES[id]();

/** Each remappable action with its default key and title, in display order. */
export const catalog = () =>
	ACTION_IDS.map((id) => ({ id, key: defaultKey(id), title: TITLES[id] }));
