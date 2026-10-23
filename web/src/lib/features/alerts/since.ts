// Where the "New since …" mark goes (§10.8). The server flags each alert `read`; alerts come
// newest first, and reading marks everything up to an id, so the unread ones are a prefix. The
// mark sits between the last unread row and the first read row after it, and its time is that
// read row's `created_at`: every alert above it is newer than the last one you had seen.
export interface Flagged {
	read: boolean;
	created_at: number;
}

/**
 * `before` is the index of the first read row (the mark goes in front of it) and `at` its
 * time. Null when no row is unread (nothing is new), or when every row loaded so far is unread
 * (the mark appears when the first read row arrives, and never if there isn't one).
 */
export function dividerBefore(rows: readonly Flagged[]): { before: number; at: number } | null {
	const lastUnread = rows.findLastIndex((r) => !r.read);
	const next = rows[lastUnread + 1];
	if (lastUnread === -1 || next === undefined) return null;
	return { before: lastUnread + 1, at: next.created_at };
}
