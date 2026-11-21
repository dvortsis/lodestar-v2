// The names and descriptions of rules and packs (`rule-text.ts`), loaded once, on demand. Until
// they're in, or when they can't load, every rule and pack is named by its code: never an empty
// label.
type Texts = typeof import('./rule-text');

let texts = $state.raw<Texts | null>(null);
let loading: Promise<void> | undefined;

/** Loads the texts; settles either way, and a failure lets a later call try again. */
export function loadNames(): Promise<void> {
	return (loading ??= import('./rule-text').then(
		(t) => {
			texts = t;
		},
		() => {
			loading = undefined;
		}
	));
}

/** The rule's or pack's plain name, or its code. */
export const nameOf = (id: string): string => texts?.nameOf(id) ?? id;
/** Whether `id` has a plain name, so the code is shown beside it rather than instead of it. */
export const named = (id: string): boolean => nameOf(id) !== id;
/** What it checks, in one line, or null. */
export const textOf = (id: string): string | null => texts?.textOf(id) ?? null;
