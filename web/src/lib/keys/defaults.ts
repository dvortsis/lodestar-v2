// The default keyboard map (architecture §10.16), keyed by command id. A feature registers a
// command with one of these ids and gets the key without repeating it; Settings can remap or turn
// off the single-key ones (§10.13, WCAG 2.1.4). A command may still pass its own `keys`.
// The single-character ids are the server's persisted shortcut ids (`DEFAULT_SHORTCUTS` in
// `crates/lodestar/src/settings/preferences.rs`); `keys.test.ts` checks the two lists agree.
export const DEFAULT_KEYS = {
	'search.focus': ['/'],
	'results.next': ['j'],
	'results.previous': ['k'],
	'detail.open': ['Enter'],
	'detail.open_magnet': ['o'],
	'detail.copy_magnet': ['c'],
	'detail.send': ['s'],
	'results.select': ['x'],
	dismiss: ['Escape'],
	'palette.open': ['mod+k'],
	'help.shortcuts': ['?']
} as const satisfies Record<string, readonly string[]>;

export type DefaultKeyId = keyof typeof DEFAULT_KEYS;
