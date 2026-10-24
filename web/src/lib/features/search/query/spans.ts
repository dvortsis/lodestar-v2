// The server reports a parse error's span as UTF-8 byte offsets into the query it parsed
// (crates/api/src/search.rs `parse_error`); the editor works in UTF-16 offsets. Out-of-range
// offsets clamp to the text, and an offset inside a code point moves to that code point's start.

const encoder = new TextEncoder();

/** UTF-16 offset of byte offset `byte` in `text`. */
export function byteToUtf16(text: string, byte: number): number {
	if (!(byte > 0)) return 0; // NaN, negative or 0
	let bytes = 0;
	let i = 0;
	while (i < text.length) {
		const cp = text.codePointAt(i)!;
		const width = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
		if (bytes + width > byte) return i;
		bytes += width;
		i += cp > 0xffff ? 2 : 1;
	}
	return text.length;
}

/**
 * The UTF-16 range to mark for a byte span: `[from, to)`. An empty span (`start === end`)
 * marks the one character at `start`; at the end of the text it stays empty (the editor then
 * marks the end itself).
 */
export function errorRange(
	text: string,
	start: number | undefined,
	end: number | undefined
): { from: number; to: number } {
	const from = byteToUtf16(text, start ?? 0);
	let to = Math.max(from, byteToUtf16(text, end ?? start ?? 0));
	if (to === from && from < text.length) to = from + (text.codePointAt(from)! > 0xffff ? 2 : 1);
	return { from, to };
}

/** UTF-8 length of `text`, for tests and for clamping against the server's byte limits. */
export const byteLength = (text: string) => encoder.encode(text).length;
