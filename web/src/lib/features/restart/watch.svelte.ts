// Tells the status pill whether saved settings wait for a restart. Asks `GET /system` on start and
// once a minute; the Settings banner shares the same store and refreshes it sooner.
import { restart } from './restart.svelte';

const EVERY_MS = 60_000;

export function watchRestart(set: (needed: boolean) => void): () => void {
	void restart.refresh();
	const every = setInterval(() => void restart.refresh(), EVERY_MS);
	const stopEffect = $effect.root(() => {
		$effect(() => set(restart.pending !== null));
	});
	return () => {
		clearInterval(every);
		stopEffect();
	};
}
