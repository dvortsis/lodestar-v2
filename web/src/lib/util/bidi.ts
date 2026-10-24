// Torrent names and paths are untrusted text. A right-to-left override (U+202E) can make
// `invoice‮fdp.exe` display as "invoiceexe.pdf", so bidirectional controls are shown as visible
// markers instead of acting on the text around them.
const BIDI = /[‪-‮⁦-⁩]/g;

/** `text` with each bidirectional control replaced by a visible `<U+202E>` marker. */
export function showBidi(text: string): string {
	return text.replace(
		BIDI,
		(c) => `<U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, '0')}>`
	);
}
