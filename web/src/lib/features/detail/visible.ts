/** Calls `onVisible` once when the node first scrolls near the viewport. Without the observer API
 * (old browsers, jsdom) the section simply loads at once. */
export function whenVisible(node: Element, onVisible: () => void) {
	if (typeof IntersectionObserver === 'undefined') {
		onVisible();
		return {};
	}
	const observer = new IntersectionObserver(
		(entries) => {
			if (entries.some((e) => e.isIntersecting)) {
				observer.disconnect();
				onVisible();
			}
		},
		{ rootMargin: '200px' }
	);
	observer.observe(node);
	return { destroy: () => observer.disconnect() };
}
