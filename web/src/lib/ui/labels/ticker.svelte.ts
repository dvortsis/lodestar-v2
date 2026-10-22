// One shared clock for every RelativeTime: a single interval runs while at least one instance is
// mounted, instead of a timer per instance. `now` is Unix seconds and re-renders subscribers.
const INTERVAL_MS = 60_000;

let subscribers = 0;
let timer: ReturnType<typeof setInterval> | undefined;

export const ticker = $state({ now: Date.now() / 1000 });

/** Returns the unsubscribe function; call it from `$effect`. */
export function subscribeTicker(): () => void {
	if (subscribers++ === 0) {
		ticker.now = Date.now() / 1000;
		timer = setInterval(() => (ticker.now = Date.now() / 1000), INTERVAL_MS);
	}
	return () => {
		if (--subscribers === 0 && timer !== undefined) {
			clearInterval(timer);
			timer = undefined;
		}
	};
}

/** For tests: how many instances are subscribed. */
export const tickerSubscribers = () => subscribers;
