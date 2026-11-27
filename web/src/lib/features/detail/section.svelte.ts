// The section a drawer open asked for (GS-5's "Show all" opens Related), kept until the drawer has
// that torrent on screen and has scrolled to it. Replaced whole on every ask, so asking again for
// the torrent already open scrolls again.
export type Section = 'related';

let asked = $state.raw<{ hash: string; section: Section } | null>(null);

/** The section the latest open asked for, or null. */
export const wanted = () => asked;

/** What the open of `hash` should scroll to (null: nothing, the top as usual). */
export function want(hash: string, section: Section | null) {
	asked = section ? { hash, section } : null;
}
