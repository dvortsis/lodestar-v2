// The action names are this chunk's own keys: sharing the Rules list's would move them into the
// first view's chunks. Plain-language names for the server's codes (§10.14). An unknown code reads as a neutral word
// rather than being shown raw or guessed at.
import { m } from '$lib/paraglide/messages.js';

const actions: Record<string, () => string> = {
	flag: m.filters_packs_act_flag,
	demote: m.filters_packs_act_demote,
	hide: m.filters_packs_act_hide,
	quarantine: m.filters_packs_act_quarantine,
	drop: m.filters_packs_act_drop
};
export const ACTIONS = ['flag', 'demote', 'hide', 'quarantine', 'drop'] as const;
export const actionText = (a: string | null) =>
	a && actions[a] ? actions[a]!() : m.filters_packs_act_unknown();

const directions: Record<string, () => string> = {
	widens: m.filters_packs_dir_widens,
	narrows: m.filters_packs_dir_narrows,
	'action-heavier': m.filters_packs_dir_heavier,
	'action-lighter': m.filters_packs_dir_lighter,
	mixed: m.filters_packs_dir_mixed
};
/** Null (no record) and anything unknown read as "not recorded": the server then asks for the password. */
export const directionText = (d: string | null) =>
	d && directions[d] ? directions[d]!() : m.filters_packs_dir_unknown();
