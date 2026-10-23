// Titles of the remappable actions. The Settings screen lists these, not `keys.list()`: on that
// page the search and detail commands are not registered.
import { m } from '$lib/paraglide/messages.js';
import type { ActionId } from './actions';

export { ACTION_IDS, defaultKey, type ActionId } from './actions';

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
