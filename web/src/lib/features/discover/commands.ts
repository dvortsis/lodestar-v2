// Discover's keys (§10.16), in the `discover` scope: j/k move through the lists. Loaded on demand
// after mount (with the key registry), so neither is in the route's first view.
import type { keys as Keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';

export function registerDiscoverCommands(
	keys: typeof Keys,
	step: (delta: number) => void
): () => void {
	const pop = keys.pushScope('discover');
	const off = [
		keys.register({
			id: 'results.next',
			title: () => m.discover_key_next(),
			section: () => m.discover_section(),
			scope: 'discover',
			palette: false,
			run: () => step(1)
		}),
		keys.register({
			id: 'results.previous',
			title: () => m.discover_key_previous(),
			section: () => m.discover_section(),
			scope: 'discover',
			palette: false,
			run: () => step(-1)
		})
	];
	return () => {
		off.forEach((f) => f());
		pop();
	};
}
