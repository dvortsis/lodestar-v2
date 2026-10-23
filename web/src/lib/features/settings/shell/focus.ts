// Opening a screen moves focus to its `<h1 tabindex="-1">` (§10.10.11), unless the URL hash names
// a field, which the screen focuses itself once it is ready.
export function focusHeading(): boolean {
	const h1 = document.querySelector<HTMLElement>('main h1');
	if (!h1) return false;
	h1.focus({ preventScroll: true });
	return true;
}
