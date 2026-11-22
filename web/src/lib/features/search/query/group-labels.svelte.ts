// Release-group labels the client already holds (X-group-chip-label): the facet rail's
// `FacetValue.label` (`rarbg` → "RARBG", the most common spelling in its torrents' names), kept
// for the page session so a query chip can show it. Only the displayed chip changes: the query
// text, the URL, copy, share and saved searches keep the value, and the Builder's input stays
// the raw query text it writes.
// - A label enters only if it is the value with ASCII letters re-cased and nothing else (S1-20,
//   as the server's `fit_label`): the same length, each character equal or an ASCII letter that
//   differs only in case. Anything else is dropped, so a label can never read as another group.
// - At most `CAP` labels, the oldest dropped first.
// - No label: the chip shows the value, as before. Nothing is derived from names here.

export const CAP = 500;

let labels = $state.raw(new Map<string, string>());

/** The label is `value` with only ASCII letters re-cased (S1-20). */
export function fitsValue(value: string, label: string): boolean {
	if (label.length !== value.length) return false;
	for (let i = 0; i < value.length; i++) {
		const a = value.charCodeAt(i);
		const b = label.charCodeAt(i);
		// Equal, or the same ASCII letter in the other case (0x20 apart, both letters).
		if (a !== b && ((a | 0x20) !== (b | 0x20) || (a | 0x20) < 97 || (a | 0x20) > 122)) return false;
	}
	return true;
}

/** Keep the labels a group facet's listed values carry that pass the check. */
export function rememberGroups(facet?: {
	values: readonly { value: string; label?: string }[];
}): void {
	let next: Map<string, string> | null = null;
	for (const { value, label } of facet?.values ?? []) {
		if (!label || label === value || !fitsValue(value, label) || labels.get(value) === label)
			continue;
		// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a copy published whole, never mutated after
		next ??= new Map(labels);
		next.delete(value); // a relabelled value moves to the newest end
		next.set(value, label);
	}
	if (!next) return;
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
