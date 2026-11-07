// The pill's reactive handle on the fleet (a module because the pill's `state` prop shadows the
// `$state` rune). Call from a component's init; polling stops when the component is destroyed.
import { watchFleet, type Fleet } from './fleet';

export function useFleet(): { readonly current: Fleet | null } {
	let fleet = $state.raw<Fleet | null>(null);
	$effect(() => watchFleet((f) => (fleet = f)));
	return {
		get current() {
			return fleet;
		}
	};
}
