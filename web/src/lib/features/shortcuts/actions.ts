// The remappable actions: the server's `DEFAULT_SHORTCUTS` (preferences.rs), in §10.16's order.
// No messages here, so boot.ts can import it without pulling the titles into its chunk.
import { DEFAULT_KEYS } from '$lib/keys/defaults';

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
