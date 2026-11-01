// The caches live for the tab's lifetime, so their single settings subscription does too.
// Loaded only when the detail feature is used; shares the existing SSE connection. The
// subscription starts once its chunk has loaded (right after mount), never before first paint.
let installed = false;
export function watchDetailVisibility() {
	if (installed) return;
	installed = true;
	void import('./visibility-events').then(
		(mod) => mod.subscribeDetailVisibility(),
		() => {
			installed = false;
		}
	);
}
