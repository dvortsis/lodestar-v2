// The first-run setup token (architecture §12, §16 M2): 16 hex digits the server prints to its
// log grouped as XXXX-XXXX-XXXX-XXXX. People copy it out of a terminal, the Unraid log window or
// a log file, so the parser accepts whatever comes along with it and returns the canonical form
// the server compares against (it only trims and upper-cases).

export const TOKEN_HINT = 'XXXX-XXXX-XXXX-XXXX';

const HEX = /^[0-9A-F]{16}$/;
/** Separators people (or terminals, or IMEs) put between the groups: spaces, dashes, dots. */
const SEPARATORS = /[\s\-‐-―−ー_.·]+/g;
/** Four groups of four inside surrounding text, e.g. a whole log line. */
const GROUPED =
	/(?<![0-9A-Z])([0-9A-F]{4})[-‐-―− ]([0-9A-F]{4})[-‐-―− ]([0-9A-F]{4})[-‐-―− ]([0-9A-F]{4})(?![0-9A-Z])/g;

const group = (hex: string) => hex.match(/.{4}/g)!.join('-');

/**
 * The canonical `XXXX-XXXX-XXXX-XXXX` token in `input`, or null when there is none or more than
 * one. Accepts any case, full-width characters, quotes, surrounding whitespace, any separators
 * between the groups (or none), and a pasted log line that contains the token.
 */
export function parseToken(input: string): string | null {
	// NFKC folds full-width digits and letters (a CJK input method) to ASCII.
	const text = input.normalize('NFKC').toUpperCase();
	const bare = text.replace(/["'`“”‘’「」]/g, '').replace(SEPARATORS, '');
	if (HEX.test(bare)) return group(bare);
	const found = new Set([...text.matchAll(GROUPED)].map((m) => m.slice(1, 5).join('-')));
	return found.size === 1 ? [...found][0]! : null;
}
