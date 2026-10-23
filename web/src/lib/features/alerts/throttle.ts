// A value that follows its source at most once per `ms`: the first change lands at once, later
// ones wait out the window and the last one wins. For the polite live region (§10.13: throttled).
export function throttled<T>(ms: number, apply: (value: T) => void, now = () => Date.now()) {
	let last = -Infinity;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let pending: { value: T } | undefined;
	function flush() {
		timer = undefined;
		if (!pending) return;
		const { value } = pending;
		pending = undefined;
		last = now();
		apply(value);
	}
	return {
		set(value: T) {
			pending = { value };
			if (timer) return;
			const wait = last + ms - now();
			if (wait <= 0) flush();
			else timer = setTimeout(flush, wait);
		},
		/** Applies `value` now without using up the window (clearing is not an announcement). */
		reset(value: T) {
			clearTimeout(timer);
			timer = undefined;
			pending = undefined;
			apply(value);
		},
		cancel() {
			clearTimeout(timer);
			timer = undefined;
			pending = undefined;
		}
	};
}
