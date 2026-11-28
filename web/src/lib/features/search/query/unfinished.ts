// An unfinished token (U8's neighbour in P10): while the caret is inside the token being typed and
// the suggestions are open, a parse error in that token is not shown, since `res:` is on its way
// to `res:1080p`. Submitting shows it at once (the caller clears `caret` then).
export interface Span {
	from: number;
	to: number;
}

/** True when the error is in the token the caret is in, and the suggestions are open. */
export function isUnfinished(error: Span | null, caret: number | null): boolean {
	if (!error || caret === null) return false;
	return caret >= error.from && caret <= error.to;
}
