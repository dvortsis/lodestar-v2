/** Make covered background branches inert, preserving each branch's prior state.
 * New popup portals remain available to menus/dialogs opened from detail. */
export function coverBackground(host: HTMLElement): () => void {
	const changed: HTMLElement[] = [];
	let branch: HTMLElement | null = host;
	while (branch?.parentElement) {
		for (const sibling of branch.parentElement.children) {
			if (
				sibling !== branch &&
				sibling instanceof HTMLElement &&
				!sibling.inert &&
				!sibling.matches('script, style, link')
			) {
				sibling.inert = true;
				changed.push(sibling);
			}
		}
		branch = branch.parentElement;
		if (branch === document.body) break;
	}
	return () =>
		changed.forEach((element) => {
			element.inert = false;
		});
}
