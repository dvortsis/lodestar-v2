// The lookups every label formatter shares (X-labels): own keys only, and the value as sent when
// a table has no entry, never an empty label.

/** Own keys only: a value the server sends is never read as `constructor` or another inherited name. */
export const own = <T>(table: Record<string, T>, key: string): T | undefined =>
	Object.hasOwn(table, key) ? table[key] : undefined;

/** The table's word for `value` (a string, or a message), else `value` itself. */
export const word = (table: Record<string, string | (() => string)>, value: string): string => {
	const hit = own(table, value);
	return hit === undefined ? value : typeof hit === 'string' ? hit : hit();
};
