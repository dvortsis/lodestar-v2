// Runs `start` the first time the event stream is live, and `stop` when the caller stops. The
// stream belongs to the shell (it opens none on the sign-in screens, and a consumer with another
// topic makes it reconnect), so this waits for it instead of opening one.
import { untrack } from 'svelte';
import { events } from '$lib/state/events.svelte';

export function onceLive(start: () => () => void): () => void {
	let stop: (() => void) | undefined;
	const root = $effect.root(() => {
		$effect(() => {
			if (events.status === 'live' && !stop) stop = untrack(start);
		});
	});
	return () => {
		root();
		stop?.();
	};
}
