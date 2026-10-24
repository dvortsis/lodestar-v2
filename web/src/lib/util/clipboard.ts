export type CopyResult = 'copied' | 'manual';

/**
 * Copies `text`. `navigator.clipboard` needs a secure context; a plain-HTTP LAN address (the usual
 * home-server setup) is not one, so the hidden-textarea `execCommand('copy')` is the normal path
 * there. `'manual'` means neither worked: show the text selected and let the user press Ctrl+C.
 */
export async function copyText(text: string): Promise<CopyResult> {
	if (window.isSecureContext && navigator.clipboard) {
		try {
			await navigator.clipboard.writeText(text);
			return 'copied';
		} catch {
			// denied or unavailable: fall through to the textarea
		}
	}
	return execCopy(text) ? 'copied' : 'manual';
}

function execCopy(text: string): boolean {
	const area = document.createElement('textarea');
	area.value = text;
	area.setAttribute('readonly', '');
	area.setAttribute('aria-hidden', 'true');
	area.style.position = 'fixed';
	area.style.insetBlockStart = '0';
	area.style.opacity = '0';
	const active = document.activeElement;
	document.body.appendChild(area);
	try {
		area.select();
		return typeof document.execCommand === 'function' && document.execCommand('copy');
	} catch {
		return false;
	} finally {
		area.remove();
		if (active instanceof HTMLElement) active.focus();
	}
}
