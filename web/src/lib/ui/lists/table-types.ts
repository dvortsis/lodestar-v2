import type { Snippet } from 'svelte';

export interface Column<R> {
	id: string;
	header: string;
	/** Plain text for the cell (already formatted through `lib/format`). */
	value?: (row: R) => string;
	/** Rich cell content; wins over `value`. */
	cell?: Snippet<[row: R]>;
	sortable?: boolean;
	align?: 'start' | 'end';
	/** CSS grid track, e.g. `2fr` or `8rem`. Default `1fr`. */
	width?: string;
	/** A column that can't be hidden in the chooser (usually the name). */
	required?: boolean;
}
export type SortState = { id: string; dir: 'asc' | 'desc' } | null;
