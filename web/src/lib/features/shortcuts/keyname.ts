// How a shortcut reads aloud. Screen readers say "slash" for `/` only some of the time and nothing
// useful for most other symbols, so the visible text is hidden from them and a name is read out.
import { formatCombo } from '$lib/keys/display';
import { m } from '$lib/paraglide/messages.js';

const NAMES: Record<string, () => string> = {
	'/': () => m.shortcuts_key_slash(),
	'?': () => m.shortcuts_key_question(),
	',': () => m.shortcuts_key_comma(),
	'.': () => m.shortcuts_key_period(),
	'-': () => m.shortcuts_key_minus(),
	'=': () => m.shortcuts_key_equals(),
	';': () => m.shortcuts_key_semicolon(),
	"'": () => m.shortcuts_key_apostrophe(),
	'\\': () => m.shortcuts_key_backslash(),
	'[': () => m.shortcuts_key_bracket_left(),
	']': () => m.shortcuts_key_bracket_right(),
	'`': () => m.shortcuts_key_backtick()
};

/** The combo as shown (`Ctrl+K`, `/`); a combo this build cannot parse is shown as it is. */
export function shown(combo: string, mac?: boolean): string {
	try {
		return formatCombo(combo, mac);
	} catch {
		return combo;
	}
}

/** The combo as read out: the shown text with a trailing symbol replaced by its name. */
export function keyNameFor(combo: string, mac?: boolean): string {
	const text = shown(combo, mac);
	const last = [...text].pop() ?? '';
	const name = NAMES[last];
	return name ? text.slice(0, text.length - last.length) + name() : text;
}
