// What the editor accepts as a shortcut key and when two actions clash. The server checks the same
// rules (`check_shortcuts` in crates/lodestar/src/settings/preferences.rs); this only explains a
// refusal before the request.
import { keys } from '$lib/keys';
import { normalizeCombo } from '$lib/keys/combo';
import { ACTION_IDS, defaultKey, type ActionId } from './catalog';

export type Refusal = 'shift' | 'modifier' | 'blank' | 'plus';
export type Captured = { key: string } | { cancel: true } | { ignore: true } | { refuse: Refusal };

// Mirrors preferences.rs's check for characters that draw nothing: format characters and combining marks, which draw nothing.
const BLANK: [number, number][] = [
	[0x00ad, 0x00ad],
	[0x034f, 0x034f],
	[0x061c, 0x061c],
	[0x115f, 0x1160],
	[0x17b4, 0x17b5],
	[0x180b, 0x180f],
	[0x200b, 0x200f],
	[0x202a, 0x202e],
	[0x2060, 0x206f],
	[0x3164, 0x3164],
	[0xfe00, 0xfe0f],
	[0xfeff, 0xfeff],
	[0xffa0, 0xffa0],
	[0xfff0, 0xfffb],
	[0x0300, 0x036f],
	[0x0483, 0x0489],
	[0x0591, 0x05bd],
	[0x0610, 0x061a],
	[0x064b, 0x065f],
	[0x1ab0, 0x1aff],
	[0x1dc0, 0x1dff],
	[0x20d0, 0x20ff],
	[0xfe20, 0xfe2f],
	[0xe0000, 0xe007f],
	[0xe0100, 0xe01ef]
];
const blank = (c: number) => BLANK.some(([lo, hi]) => c >= lo && c <= hi);
const MODIFIER_KEYS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'AltGraph', 'CapsLock', 'OS']);

/** One keydown, read as a candidate shortcut. */
export function capture(
	e: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'altKey' | 'metaKey' | 'shiftKey'>
): Captured {
	if (e.key === 'Escape' || e.key === 'Tab') return { cancel: true };
	if (MODIFIER_KEYS.has(e.key)) return { ignore: true };
	const chars = [...e.key];
	if (chars.length !== 1 || /[\p{Cc}\s]/u.test(e.key) || blank(e.key.codePointAt(0)!))
		return { refuse: 'blank' };
	if (e.ctrlKey || e.altKey || e.metaKey) return { refuse: 'modifier' };
	// The registry folds case, so Shift+J would act as plain j.
	const letter = e.key.toLowerCase() !== e.key.toUpperCase();
	if (e.shiftKey && letter) return { refuse: 'shift' };
	// "+" separates a combo's parts, so the registry cannot hold it as a key.
	if (e.key === '+') return { refuse: 'plus' };
	const lower = e.key.toLowerCase();
	return { key: [...lower].length === 1 ? lower : e.key };
}

export type KeyMap = Record<string, string>;

/** An action's key: its remap, else its default. */
export const effective = (map: KeyMap, id: ActionId): string => map[id] ?? defaultKey(id);

/** The other action that already has `key`, if any (the server refuses two actions on one key). */
export function clash(map: KeyMap, id: ActionId, key: string): ActionId | undefined {
	return ACTION_IDS.find((other) => other !== id && effective(map, other) === key);
}

/** Sets an action's key; a key equal to its default drops the remap. */
export function withKey(map: KeyMap, id: ActionId, key: string): KeyMap {
	const next = { ...map };
	if (key === defaultKey(id)) delete next[id];
	else next[id] = key;
	return next;
}

/** Swaps: `id` takes `key`, and the action that had it takes `id`'s old key. */
export function swap(map: KeyMap, id: ActionId, other: ActionId, key: string): KeyMap {
	return withKey(withKey(map, other, effective(map, id)), id, key);
}

/** Registered commands that are not remappable actions but answer to the same bare key. */
export function collisions(key: string) {
	let combo: string;
	try {
		combo = normalizeCombo(key);
	} catch {
		return [];
	}
	return keys
		.list()
		.filter((c) => !(ACTION_IDS as readonly string[]).includes(c.id) && c.keys.includes(combo));
}
