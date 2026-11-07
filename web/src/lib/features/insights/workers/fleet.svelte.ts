// The pill's handle on the fleet: polling, plus the visible text (" N workers · X/h") and the
// description (paused and offline). A module because the pill's `state` prop shadows the `$state`
// rune. Call from a component's init; polling stops when the component is destroyed.
import { formatCompact, formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import { watchFleet, type Fleet } from './fleet';

/** `active`: the pill is in a state that shows its count, so the fleet text belongs after it. */
export function useFleet(active: () => boolean): { readonly text: string; readonly help: string } {
	let fleet = $state.raw<Fleet | null>(null);
	$effect(() => watchFleet((f) => (fleet = f)));
	const locale = $derived(currentLocale());
	const text = $derived(
		fleet && active()
			? [
					(fleet.workers === 1
						? m.insights_workers_pill_workers_one
						: m.insights_workers_pill_workers)({ count: formatCount(fleet.workers, locale) }),
					fleet.stored_per_hour === null
						? ''
						: m.insights_workers_pill_rate({ rate: formatCompact(fleet.stored_per_hour, locale) })
				]
					.filter(Boolean)
					.join(' · ')
			: ''
	);
	const help = $derived(
		fleet && active()
			? m.insights_workers_pill_detail({
					workers: formatCount(fleet.workers, locale),
					paused: formatCount(fleet.paused, locale),
					offline: formatCount(fleet.offline, locale)
				})
			: ''
	);
	return {
		get text() {
			return text;
		},
		get help() {
			return help;
		}
	};
}
