// A component loaded the first time it's wanted: what a section's first view doesn't show.
export function lazyPart<T>(load: () => Promise<T>) {
	let part = $state.raw<T>();
	let failed = $state(false);
	let started = false;
	return {
		/** The component, once loaded (reactive). */
		get part() {
			return part;
		},
		/** Its code didn't load (reactive). Browsers keep a failed module, so only a reload helps. */
		get failed() {
			return failed;
		},
		/** Starts the load when `when` holds, once. */
		want(when: unknown) {
			if (!when || started) return;
			started = true;
			load().then(
				(loaded) => (part = loaded),
				() => (failed = true)
			);
		}
	};
}
