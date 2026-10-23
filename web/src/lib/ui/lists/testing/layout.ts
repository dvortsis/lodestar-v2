// jsdom has no layout: the virtualiser reads the scroller's clientHeight and watches it with a
// ResizeObserver. Call `stubLayout()` in a test file's beforeAll to give every element a size.
export function stubLayout(height = 360, width = 800) {
	Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
		configurable: true,
		value: height
	});
	Object.defineProperty(HTMLElement.prototype, 'clientWidth', { configurable: true, value: width });
	Object.defineProperty(HTMLElement.prototype, 'offsetHeight', { configurable: true, value: 36 });
	Object.defineProperty(HTMLElement.prototype, 'scrollTo', { configurable: true, value: () => {} });
	globalThis.ResizeObserver ??= class {
		observe() {}
		unobserve() {}
		disconnect() {}
	};
}
