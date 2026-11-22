// Release-group labels the client already holds (X-group-chip-label): the facet rail's
// `FacetValue.label` (`rarbg` → "RARBG", the most common spelling in its torrents' names), kept
// for the page session so a query chip can show it. Only the displayed chip changes: the query
// text, the URL, copy, share and saved searches keep the value, and the Builder's input stays
// the raw query text it writes.
// - A label enters only if it is the value with some ASCII letters in upper case and nothing else
//   (S1-20, as the server's `fit_label`): the same length, each character the value's own or the
//   upper case of a lower-case ASCII letter there. Anything else is dropped, so a label can never
//   read as another group.
// - The latest response wins: a value it lists with no label (the server sends none when the
//   winning spelling is the value) or a failing one loses its label; every value it lists moves to
//   the newest end, so that response can't evict it. At most `CAP` labels, the oldest dropped.
// - No label: the chip shows the value, as before. Nothing is derived from names here.

export const CAP = 500;

let labels = $state.raw(new Map<string, string>());

/** The label is `value` with only ASCII letters put in upper case (S1-20, `fit_label`). */
export function fitsValue(value: string, label: string): boolean {
	if (label.length !== value.length) return false;
	for (let i = 0; i < value.length; i++) {
		const v = value.charCodeAt(i);
		const l = label.charCodeAt(i);
		// The value's own character, or an upper-case ASCII letter whose lower case is the value's.
		if (l !== v && !(l >= 65 && l <= 90 && l + 32 === v)) return false;
	}
	return true;
}

/** Take a group facet's listed values: their passing labels, newest last; others lose theirs. */
export function rememberGroups(facet?: {
	values: readonly { value: string; label?: string }[];
}): void {
	const values = facet?.values ?? [];
	if (!values.length) return;
	// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a copy published whole, never mutated after
	const next = new Map(labels);
	for (const { value, label } of values) {
		next.delete(value); // re-seen: to the newest end, or gone when it has no passing label
		if (label && label !== value && fitsValue(value, label)) next.set(value, label);
	}
	for (const oldest of next.keys()) {
		if (next.size <= CAP) break;
		next.delete(oldest);
	}
	labels = next;
}

/** The label to show for a release group, or the value itself when the client holds none. */
export const groupText = (value: string): string => labels.get(value) ?? value;

/** Tests only: forget every label. */
export function resetGroupLabels(): void {
	// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a copy published whole, never mutated after
	labels = new Map();
}
