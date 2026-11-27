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

/**
 * Scrolls `section` into view inside `root`, then moves focus to its heading once the section has
 * mounted (Related's chunk loads when it first scrolls near), so a keyboard or screen-reader user
 * lands where Show all took them (§10.5). Nothing to wait for if no heading ever appears (a
 * withheld answer shows nothing); the returned function stops waiting.
 */
export function revealSection(root: HTMLElement, section: Section): () => void {
	const mount = root.querySelector(`[data-section="${section}"]`);
	mount?.scrollIntoView?.({ block: 'start' });
	const focus = () =>
		mount?.querySelector<HTMLElement>('[role="heading"][tabindex]')?.focus({ preventScroll: true });
	focus();
	if (!mount || mount.contains(document.activeElement)) return () => {};
	const watch = new MutationObserver(() => {
		focus();
		if (mount.contains(document.activeElement)) watch.disconnect();
	});
	watch.observe(mount, { childList: true, subtree: true });
	return () => watch.disconnect();
}
