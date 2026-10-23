// The default keyboard map (architecture §10.16), keyed by command id. A feature registers a
// command with one of these ids and gets the key without repeating it; Settings can remap or turn
// off the single-key ones (§10.13, WCAG 2.1.4). A command may still pass its own `keys`.
export const DEFAULT_KEYS = {
	'search.focus': ['/'],
	'list.next': ['j'],
	'list.previous': ['k'],
	'detail.open': ['Enter'],
	'magnet.open': ['o'],
	'magnet.copy': ['c'],
	'client.send': ['s'],
	'row.select': ['x'],
	dismiss: ['Escape'],
	'palette.open': ['mod+k'],
	'shortcuts.open': ['?']
} as const satisfies Record<string, readonly string[]>;

export type DefaultKeyId = keyof typeof DEFAULT_KEYS;
