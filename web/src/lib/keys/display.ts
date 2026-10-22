// Shortcut text for the UI. Separate from the matcher so a screen that only shows a hint loads
// this and nothing else.
import { isMacPlatform, normalizeCombo } from './combo';

const NAMES: Record<string, string> = {
	escape: 'Esc',
	enter: 'Enter',
	space: 'Space',
	arrowup: '↑',
	arrowdown: '↓',
	arrowleft: '←',
	arrowright: '→',
	tab: 'Tab',
	backspace: 'Backspace',
	delete: 'Del'
};

/** Display text: `⌘K` on macOS, `Ctrl+K` elsewhere, `/`, `Esc`. */
export function formatCombo(spec: string, mac = isMacPlatform()): string {
	const combo = normalizeCombo(spec, mac);
	const parts = combo.split('+');
	const key = parts.pop() ?? '';
	const shown = NAMES[key] ?? (key.length === 1 ? key.toUpperCase() : key);
	const sym: Record<string, [string, string]> = {
		alt: ['⌥', 'Alt'],
		ctrl: ['⌃', 'Ctrl'],
		meta: ['⌘', 'Win'],
		shift: ['⇧', 'Shift']
	};
	if (mac) return [...parts.map((p) => sym[p]![0]), shown].join('');
	return [...parts.map((p) => (p === 'meta' ? 'Meta' : sym[p]![1])), shown].join('+');
}
