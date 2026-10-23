// A component loaded the first time it's wanted: what a section's first view doesn't show.
// `peek` returns it at once when its chunk is already in, so it renders in the same update as
// what showed it (a draft offer with the fields, a staged reset with the edit), with no shift.
export function lazyPart<T>(load: () => Promise<T>, peek?: () => T | undefined) {
	let part = $state.raw<T>();
	let failed = $state(false);
	let started = false;
	return {
		/** The component, once loaded (reactive). */
		get part() {
			return part ?? peek?.();
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
