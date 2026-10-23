// Writes text to the clipboard. The Clipboard API needs a secure context, which a plain-HTTP
// LAN address (the common self-hosted case) is not, so fall back to a hidden textarea, and let
// the caller show the text for a manual copy when both fail. Promoted to `lib/` when S-detail's
// own Copy magnet exists (§10.11's "two places" rule).

export async function copyText(text: string): Promise<boolean> {
	try {
		if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
			await navigator.clipboard.writeText(text);
			return true;
		}
	} catch {
		// denied or unavailable: try the textarea
	}
	return copyViaTextarea(text);
}

function copyViaTextarea(text: string): boolean {
	if (typeof document === 'undefined' || typeof document.execCommand !== 'function') return false;
	const field = document.createElement('textarea');
	field.value = text;
	field.setAttribute('readonly', '');
	field.setAttribute('aria-hidden', 'true');
	field.tabIndex = -1;
	field.style.position = 'fixed';
	field.style.opacity = '0';
	const before = document.activeElement;
	document.body.append(field);
	try {
		field.select();
		return document.execCommand('copy');
	} catch {
		return false;
	} finally {
		field.remove();
		if (before instanceof HTMLElement) before.focus({ preventScroll: true });
	}
}

/** "Ctrl+C", or "⌘C" on a Mac. */
export function copyShortcut(): string {
	const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);
	return mac ? '⌘C' : 'Ctrl+C';
}
