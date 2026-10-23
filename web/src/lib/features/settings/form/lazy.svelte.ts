// A component loaded the first time it's wanted: what a section's first view doesn't show.
export function lazyPart<T>(load: () => Promise<T>) {
	let part = $state.raw<T>();
	let started = false;
	return {
		/** The component, once loaded (reactive). */
		get part() {
			return part;
		},
		/** Starts the load when `when` holds (once; a failed load is tried again next time). */
		want(when: unknown) {
			if (!when || started) return;
			started = true;
			load().then(
				(loaded) => (part = loaded),
				() => (started = false)
			);
		}
	};
}
