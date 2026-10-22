// Key combos: a spec is `mod+k`, `/`, `?`, `Escape`, `ctrl+shift+p`. `mod` is Cmd on macOS and
// Ctrl elsewhere. Both specs and keyboard events reduce to one canonical string, modifiers in the
// order alt, ctrl, meta, shift, then the lower-cased key, so equal combos compare with `===`.

const MODIFIERS = ['alt', 'ctrl', 'meta', 'shift'] as const;
type Modifier = (typeof MODIFIERS)[number];

export function isMacPlatform(): boolean {
	if (typeof navigator === 'undefined') return false;
	const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
	return /mac|iphone|ipad/i.test(nav.userAgentData?.platform ?? nav.platform ?? '');
}

const KEY_ALIASES: Record<string, string> = {
	esc: 'escape',
	return: 'enter',
	' ': 'space',
	spacebar: 'space',
	up: 'arrowup',
	down: 'arrowdown',
	left: 'arrowleft',
	right: 'arrowright',
	del: 'delete'
};

function canonical(mods: Set<Modifier>, key: string): string {
	return [...MODIFIERS.filter((m) => mods.has(m)), key].join('+');
}

function keyName(key: string): string {
	const k = key.toLowerCase();
	return KEY_ALIASES[k] ?? k;
}

/** Reduces a spec like `mod+k` to its canonical form for this platform. */
export function normalizeCombo(spec: string, mac = isMacPlatform()): string {
	const parts = spec.split('+').filter((p) => p !== '');
	const key = parts.pop();
	if (!key) throw new Error(`empty key combo "${spec}"`);
	const mods = new Set<Modifier>();
	for (const raw of parts) {
		const p = raw.toLowerCase();
		if (p === 'mod') mods.add(mac ? 'meta' : 'ctrl');
		else if (p === 'cmd' || p === 'command') mods.add('meta');
		else if (p === 'control') mods.add('ctrl');
		else if (p === 'option') mods.add('alt');
		else if ((MODIFIERS as readonly string[]).includes(p)) mods.add(p as Modifier);
		else throw new Error(`unknown modifier "${raw}" in key combo "${spec}"`);
	}
	const name = keyName(key);
	// A printable character already carries its shift ("?" is shift+/), so Shift alone is dropped.
	if (name.length === 1 && mods.size === 1 && mods.has('shift')) mods.delete('shift');
	return canonical(mods, name);
}

/** The canonical combo a keydown event produces. */
export function comboFromEvent(
	e: Pick<KeyboardEvent, 'key' | 'altKey' | 'ctrlKey' | 'metaKey' | 'shiftKey'>
): string {
	const mods = new Set<Modifier>();
	if (e.altKey) mods.add('alt');
	if (e.ctrlKey) mods.add('ctrl');
	if (e.metaKey) mods.add('meta');
	const name = keyName(e.key);
	// Shift is kept for named keys (shift+tab) and for printable keys only beside another modifier.
	if (e.shiftKey && (name.length > 1 || mods.size > 0)) mods.add('shift');
	return canonical(mods, name);
}

/** True when the combo is a bare printable character: the keys WCAG 2.1.4 lets people turn off. */
export function isSingleKey(combo: string): boolean {
	return combo.length === 1;
}

/** Whether the combo carries Ctrl, Alt or Meta. */
export function hasCommandModifier(combo: string): boolean {
	return /(^|\+)(alt|ctrl|meta)\+/.test(combo);
}
